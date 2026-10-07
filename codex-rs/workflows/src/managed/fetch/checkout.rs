use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;
use semver::Version;

use super::FetchedWorkflowRelease;
use super::VerificationLimits;

#[allow(dead_code, reason = "used by the managed installation stage")]
pub(in crate::managed) struct StagedWorkflowRelease {
    temporary: tempfile::TempDir,
    root: AbsolutePathBuf,
    release: super::ResolvedWorkflowRelease,
    dependencies: crate::managed::dependencies::ValidatedManagedDependencies,
    baseline: crate::managed::integrity::SourceIntegrityBaseline,
}

#[allow(dead_code, reason = "used by the managed installation stage")]
impl StagedWorkflowRelease {
    pub(in crate::managed) fn root(&self) -> &AbsolutePathBuf {
        &self.root
    }

    pub(in crate::managed) fn release(&self) -> &super::ResolvedWorkflowRelease {
        &self.release
    }

    pub(in crate::managed) fn dependencies(
        &self,
    ) -> &crate::managed::dependencies::ValidatedManagedDependencies {
        &self.dependencies
    }

    pub(in crate::managed) fn baseline(
        &self,
    ) -> &crate::managed::integrity::SourceIntegrityBaseline {
        &self.baseline
    }
}

pub(super) fn checkout_fetched_release(
    git: &OsStr,
    fetched: FetchedWorkflowRelease,
    limits: VerificationLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<StagedWorkflowRelease> {
    super::ensure_not_cancelled(cancelled)?;
    let FetchedWorkflowRelease {
        temporary,
        repository,
        release,
    } = fetched;
    let commit = &release.advertised_object_id;
    let checkout = checkout_command(git, temporary.path(), &repository, commit);
    super::run_git(checkout, "Git workflow checkout", cancelled)?;

    let mut inspect = super::repository_command(git, temporary.path(), &repository);
    inspect.args(["rev-parse", "--verify", "HEAD^{commit}"]);
    let checked_out = super::run_git(inspect, "Git workflow checkout inspection", cancelled)?;
    let checked_out = std::str::from_utf8(&checked_out)
        .context("Git workflow checkout inspection returned non-UTF-8 output")?
        .trim();
    if !checked_out.eq_ignore_ascii_case(commit) {
        bail!("Git checked out an unexpected workflow commit");
    }

    inspect_worktree(&repository, limits.worktree_entries, cancelled)?;
    super::inspect_staging(temporary.path(), limits, cancelled)?;
    let package = crate::WorkflowPackage::load(&repository)?;
    validate_package_version(&package, &release)?;
    let dependencies = crate::managed::dependencies::validate_managed_dependencies(&package)?;
    let baseline = crate::managed::integrity::capture_source_baseline(
        git,
        repository.as_path(),
        commit,
        limits,
        cancelled,
    )?;
    super::ensure_not_cancelled(cancelled)?;
    Ok(StagedWorkflowRelease {
        temporary,
        root: repository,
        release,
        dependencies,
        baseline,
    })
}

fn checkout_command(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    commit: &str,
) -> std::process::Command {
    let mut command = super::repository_command(git, working_directory, repository);
    command
        .args(["checkout", "--quiet", "--detach"])
        .arg(commit)
        .arg("--");
    command
}

fn inspect_worktree(
    root: &Path,
    maximum_entries: usize,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            super::ensure_not_cancelled(cancelled)?;
            let entry = entry?;
            if directory == root && entry.file_name() == ".git" {
                continue;
            }
            entries += 1;
            if entries > maximum_entries {
                bail!("workflow worktree exceeds {maximum_entries} entries");
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || super::is_windows_reparse_point(&metadata) {
                bail!("workflow worktree contains a symbolic link or reparse point");
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if !metadata.is_file() {
                bail!("workflow worktree contains a special file");
            }
        }
    }
    Ok(())
}

fn validate_package_version(
    package: &crate::WorkflowPackage,
    release: &super::ResolvedWorkflowRelease,
) -> anyhow::Result<()> {
    let Some(expected) = release.version.as_ref() else {
        return Ok(());
    };
    let version = package
        .package_json
        .get("version")
        .and_then(serde_json::Value::as_str)
        .context("tagged workflow release requires a string package.json version")?;
    let version = Version::parse(version)
        .with_context(|| format!("workflow package version `{version}` is not SemVer"))?;
    if &version != expected {
        bail!("workflow package version `{version}` does not match release version `{expected}`");
    }
    Ok(())
}

#[cfg(test)]
#[path = "checkout_tests.rs"]
mod tests;
