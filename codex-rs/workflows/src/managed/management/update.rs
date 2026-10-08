use std::cmp::Ordering;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use semver::Version;

use super::ManagedWorkflowService;
use crate::managed::ResolvedWorkflowRelease;
use crate::managed::WorkflowGitSource;
use crate::managed::store::ManagedWorkflowReceipt;

pub use crate::managed::store::WorkflowUpdatePolicy;

/// The exact release stored in a managed workflow receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowReleaseIdentity {
    pub tag: Option<String>,
    pub version: Option<String>,
    pub commit: String,
}

/// A bounded receipt view shared by CLI and app-server management surfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedWorkflowRecord {
    pub id: String,
    pub source: String,
    pub installed: WorkflowReleaseIdentity,
    pub policy: WorkflowUpdatePolicy,
    pub dismissed_release: Option<WorkflowReleaseIdentity>,
}

/// Result of checking one installed workflow source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManagedWorkflowUpdate {
    Current,
    Available {
        release: WorkflowReleaseIdentity,
        dismissed: bool,
    },
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedWorkflowUpdateCheck {
    pub workflow: ManagedWorkflowRecord,
    pub update: ManagedWorkflowUpdate,
}

impl ManagedWorkflowService {
    /// Lists validated managed receipts in stable ID order.
    pub fn list_installed(&self) -> anyhow::Result<Vec<ManagedWorkflowRecord>> {
        Ok(self
            .store
            .list_receipts(/*cancelled*/ None)?
            .into_iter()
            .map(ManagedWorkflowRecord::from)
            .collect())
    }

    /// Checks one source and returns a diagnostic without changing its installed release.
    pub fn check_update(
        &self,
        id: &str,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<ManagedWorkflowUpdateCheck> {
        let receipt = self.store.read_managed_receipt(id, cancelled)?;
        let workflow = ManagedWorkflowRecord::from(receipt.clone());
        let update = match check_release(&receipt, cancelled) {
            Ok(update) => update,
            Err(error) => ManagedWorkflowUpdate::Error(format!("{error:#}")),
        };
        Ok(ManagedWorkflowUpdateCheck { workflow, update })
    }
}

impl From<ManagedWorkflowReceipt> for ManagedWorkflowRecord {
    fn from(receipt: ManagedWorkflowReceipt) -> Self {
        Self {
            id: receipt.id,
            source: receipt.source,
            installed: WorkflowReleaseIdentity {
                tag: receipt.installed.tag,
                version: receipt.installed.version,
                commit: receipt.installed.commit,
            },
            policy: receipt.policy,
            dismissed_release: receipt
                .dismissed_release
                .map(|release| WorkflowReleaseIdentity {
                    tag: release.tag,
                    version: release.version,
                    commit: release.commit,
                }),
        }
    }
}

impl From<ResolvedWorkflowRelease> for WorkflowReleaseIdentity {
    fn from(release: ResolvedWorkflowRelease) -> Self {
        Self {
            tag: release.tag,
            version: release.version.map(|version| version.to_string()),
            commit: release.advertised_object_id,
        }
    }
}

fn check_release(
    receipt: &ManagedWorkflowReceipt,
    cancelled: &AtomicBool,
) -> anyhow::Result<ManagedWorkflowUpdate> {
    let source = WorkflowGitSource::parse(&receipt.source)
        .context("managed workflow source is unavailable")?;
    let latest = super::super::git_command::resolve_workflow_git_release(&source, Some(cancelled))?;
    let selected = WorkflowReleaseIdentity::from(latest);
    let installed = WorkflowReleaseIdentity {
        tag: receipt.installed.tag.clone(),
        version: receipt.installed.version.clone(),
        commit: receipt.installed.commit.clone(),
    };
    if let (Some(old), Some(new)) = (&installed.version, &selected.version)
        && Version::parse(new)?.cmp_precedence(&Version::parse(old)?) == Ordering::Less
    {
        bail!("managed workflow source now advertises a downgrade");
    }
    if let Some(tag) = &receipt.installed.tag {
        let installed_tag =
            super::super::git_command::resolve_installed_workflow_tag(&source, tag, cancelled)?;
        if !receipt
            .installed
            .commit
            .eq_ignore_ascii_case(&installed_tag.advertised_object_id)
        {
            bail!("managed workflow release tag moved to a different commit");
        }
    }
    if installed == selected
        || installed.version == selected.version
            && installed.commit.eq_ignore_ascii_case(&selected.commit)
    {
        return Ok(ManagedWorkflowUpdate::Current);
    }
    match (&installed.version, &selected.version) {
        (Some(old), Some(new)) => {
            let old = Version::parse(old)?;
            let new = Version::parse(new)?;
            match new.cmp_precedence(&old) {
                Ordering::Less => bail!("managed workflow source now advertises a downgrade"),
                Ordering::Equal => {
                    bail!("managed workflow release tag moved to a different commit")
                }
                Ordering::Greater => {}
            }
        }
        (Some(_), None) => bail!("managed workflow source no longer advertises a stable release"),
        (None, Some(_)) | (None, None) => {}
    }
    let dismissed = receipt.dismissed_release.as_ref().is_some_and(|dismissed| {
        dismissed.tag == selected.tag
            && dismissed.version == selected.version
            && dismissed.commit.eq_ignore_ascii_case(&selected.commit)
    });
    Ok(ManagedWorkflowUpdate::Available {
        release: selected,
        dismissed,
    })
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
