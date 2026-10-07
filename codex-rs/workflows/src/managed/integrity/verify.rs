use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;

use super::PayloadEntryKind;
use super::PayloadInventory;
use super::PayloadKind;
use super::scan_payload;
use crate::managed::fetch::StagedWorkflowRelease;
use crate::managed::fetch::VerificationLimits;

const EVIDENCE_FORMAT_VERSION: u32 = 1;
const GIT_STDERR_LIMIT: usize = 4 * 1024;

/// Canonical identity of a complete activation payload, including empty directories.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::managed) struct ActivationPayloadEvidence {
    pub(in crate::managed) format_version: u32,
    pub(in crate::managed) sha256: String,
    pub(in crate::managed) entry_count: u64,
    pub(in crate::managed) logical_bytes: u64,
}

/// A staged release whose Git source and complete activation payload passed verification.
pub(in crate::managed) struct VerifiedWorkflowRelease {
    staged: StagedWorkflowRelease,
    evidence: ActivationPayloadEvidence,
}

impl VerifiedWorkflowRelease {
    pub(in crate::managed) fn evidence(&self) -> &ActivationPayloadEvidence {
        &self.evidence
    }

    pub(in crate::managed) fn into_staged_and_evidence(
        self,
    ) -> (StagedWorkflowRelease, ActivationPayloadEvidence) {
        (self.staged, self.evidence)
    }
}

pub(in crate::managed) fn verify_post_install(
    staged: StagedWorkflowRelease,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<VerifiedWorkflowRelease> {
    deadline.check(cancelled)?;
    let root = staged.root().as_path();
    check_git_state(root, staged.baseline(), deadline, cancelled)?;
    let inventory = scan_payload(root, PayloadKind::Staged, limits, deadline, cancelled)?;
    let has_node_modules = inventory.entries.iter().any(|entry| {
        entry.path == "node_modules" && matches!(entry.kind, PayloadEntryKind::Directory)
    });
    if has_node_modules != staged.dependencies().sources.has_dependencies {
        bail!("workflow dependency directory does not match package requirements");
    }
    let source = inventory
        .entries
        .iter()
        .filter(|entry| entry.path != "node_modules" && !entry.path.starts_with("node_modules/"))
        .collect::<Vec<_>>();
    if source != staged.baseline().source.entries.iter().collect::<Vec<_>>() {
        bail!("workflow source payload changed after checkout");
    }
    check_git_state(root, staged.baseline(), deadline, cancelled)?;
    let evidence = evidence_for_inventory(&inventory)?;
    deadline.check(cancelled)?;
    Ok(VerifiedWorkflowRelease { staged, evidence })
}

/// Rechecks a copied pending payload before the lifecycle transaction publishes it.
pub(in crate::managed) fn verify_materialized_copy(
    root: &Path,
    expected: &ActivationPayloadEvidence,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    verify_copy(
        root,
        expected,
        PayloadKind::Installed,
        limits,
        deadline,
        cancelled,
    )
}

pub(in crate::managed) fn verify_published_copy(
    root: &Path,
    expected: &ActivationPayloadEvidence,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    verify_copy(
        root,
        expected,
        PayloadKind::Published,
        limits,
        deadline,
        cancelled,
    )
}

pub(in crate::managed) fn backup_payload_evidence(
    root: &Path,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ActivationPayloadEvidence> {
    let inventory = scan_payload(root, PayloadKind::Backup, limits, deadline, cancelled)?;
    evidence_for_inventory(&inventory)
}

pub(in crate::managed) fn published_payload_evidence(
    root: &Path,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ActivationPayloadEvidence> {
    let inventory = scan_payload(root, PayloadKind::Published, limits, deadline, cancelled)?;
    evidence_for_inventory(&inventory)
}

fn verify_copy(
    root: &Path,
    expected: &ActivationPayloadEvidence,
    kind: PayloadKind,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    if expected.format_version != EVIDENCE_FORMAT_VERSION {
        bail!("unsupported workflow payload evidence version");
    }
    let inventory = scan_payload(root, kind, limits, deadline, cancelled)?;
    let actual = evidence_for_inventory(&inventory)?;
    if &actual != expected {
        bail!("copied workflow payload differs from verified release");
    }
    Ok(())
}

fn evidence_for_inventory(
    inventory: &PayloadInventory,
) -> anyhow::Result<ActivationPayloadEvidence> {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-managed-workflow-payload\0v1\0");
    let mut entries = inventory.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    for entry in &entries {
        hash_length_prefixed(&mut hasher, entry.path.as_bytes());
        match &entry.kind {
            PayloadEntryKind::Directory => hasher.update([0]),
            PayloadEntryKind::File { executable, sha256 } => {
                hasher.update([1, u8::from(*executable)]);
                hasher.update(sha256);
            }
            PayloadEntryKind::DependencyLink { target } => {
                hasher.update([2]);
                hash_length_prefixed(&mut hasher, target.as_bytes());
            }
        }
    }
    let digest = hasher.finalize();
    Ok(ActivationPayloadEvidence {
        format_version: EVIDENCE_FORMAT_VERSION,
        sha256: format!("{digest:x}"),
        entry_count: u64::try_from(entries.len()).context("workflow entry count overflow")?,
        logical_bytes: inventory.logical_bytes,
    })
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn check_git_state(
    root: &Path,
    baseline: &super::SourceIntegrityBaseline,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let working_directory = root.parent().context("workflow checkout has no parent")?;
    let git = OsStr::new("git");
    let mut head = crate::managed::fetch::repository_command(git, working_directory, root);
    head.args(["rev-parse", "--verify", "HEAD^{commit}"]);
    let head = run_git_until(head, "workflow HEAD inspection", deadline, cancelled)?;
    let head = std::str::from_utf8(&head)
        .context("workflow HEAD was not UTF-8")?
        .trim();
    if !head.eq_ignore_ascii_case(&baseline.commit) {
        bail!("workflow HEAD changed after checkout");
    }
    let mut index = crate::managed::fetch::repository_command(git, working_directory, root);
    index.args(["ls-files", "--stage", "-z", "--cached", "--full-name"]);
    let index = run_git_until(index, "workflow index inspection", deadline, cancelled)?;
    if super::parse_index(&index, baseline.commit.len())? != baseline.index {
        bail!("workflow Git index changed after checkout");
    }
    if super::hash_regular_file(
        &root.join(".git/index"),
        super::MAX_GIT_INDEX_BYTES,
        deadline,
        cancelled,
    )?
    .0 != baseline.index_sha256
    {
        bail!("workflow Git index bytes changed after checkout");
    }
    for args in [
        &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "--"][..],
        &[
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            "HEAD",
            "--",
        ][..],
    ] {
        let mut diff = crate::managed::fetch::repository_command(git, working_directory, root);
        diff.args(args);
        run_git_until(diff, "workflow Git source inspection", deadline, cancelled)?;
    }
    Ok(())
}

fn run_git_until(
    command: Command,
    description: &str,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<Vec<u8>> {
    let output = crate::runner::run_bounded_command_until_with_limits(
        command,
        deadline,
        crate::runner::CommandOutputLimits {
            stdout_bytes: crate::managed::git_command::MAX_GIT_OUTPUT_BYTES,
            stderr_bytes: GIT_STDERR_LIMIT,
        },
        cancelled,
    )
    .with_context(|| format!("{description} could not complete"))?;
    if output.stdout_oversized || output.stderr_oversized || !output.status.success() {
        bail!("{description} failed or exceeded its output limit");
    }
    Ok(output.stdout)
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
