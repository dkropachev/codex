use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::ResolvedWorkflowRelease;
use super::WorkflowGitSource;

const RELEASE_REF: &str = "refs/codex/workflow-release";
const SOURCE_REMOTE: &str = "codex-workflow-source";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const BLOB_FILTER_BYTES: u64 = 128 * 1024 * 1024 + 1;
#[allow(dead_code, reason = "used by the managed installation stage")]
pub(super) struct FetchedWorkflowRelease {
    _temporary: tempfile::TempDir,
    pub(super) repository: AbsolutePathBuf,
    pub(super) release: ResolvedWorkflowRelease,
}

#[allow(dead_code, reason = "used by the managed installation stage")]
pub(super) fn fetch_resolved_workflow_release_cancellable(
    staging_root: &AbsolutePathBuf,
    source: &WorkflowGitSource,
    release: &ResolvedWorkflowRelease,
    cancelled: &AtomicBool,
) -> anyhow::Result<FetchedWorkflowRelease> {
    fetch_with_options(
        OsStr::new("git"),
        staging_root,
        source,
        release,
        Some(cancelled),
        BLOB_FILTER_BYTES,
    )
}

fn fetch_with_options(
    git: &OsStr,
    staging_root: &AbsolutePathBuf,
    source: &WorkflowGitSource,
    release: &ResolvedWorkflowRelease,
    cancelled: Option<&AtomicBool>,
    blob_filter_bytes: u64,
) -> anyhow::Result<FetchedWorkflowRelease> {
    ensure_not_cancelled(cancelled)?;
    release.validate_identity()?;
    fs::create_dir_all(staging_root).context("failed to create workflow staging root")?;
    if !fs::symlink_metadata(staging_root)?.is_dir() {
        bail!("workflow staging root must be a regular directory");
    }

    let temporary = tempfile::Builder::new()
        .prefix("workflow-git-")
        .tempdir_in(staging_root)
        .context("failed to create workflow Git staging directory")?;
    let repository = temporary.path().join("repository");
    let template = temporary.path().join("empty-template");
    fs::create_dir(&template).context("failed to create empty Git template")?;
    let object_format = match release.advertised_object_id.len() {
        40 => "sha1",
        64 => "sha256",
        _ => bail!("resolved workflow release has an invalid advertised object ID"),
    };
    let mut init = super::git_command::trusted_git_command(git, temporary.path());
    init.args(["init", "--quiet"])
        .arg(format!("--object-format={object_format}"))
        .arg("--template")
        .arg(&template)
        .arg(&repository);
    run_git(init, "Git repository initialization", cancelled)?;
    fs::remove_dir(&template).context("failed to remove empty Git template")?;

    let mut remote = repository_command(git, temporary.path(), &repository);
    remote
        .args(["remote", "add", SOURCE_REMOTE])
        .arg(source.as_os_str());
    run_git(remote, "Git workflow source setup", cancelled)?;
    run_git(
        fetch_command(
            git,
            temporary.path(),
            &repository,
            release,
            blob_filter_bytes,
        ),
        "Git workflow release fetch",
        cancelled,
    )?;

    let mut remove = repository_command(git, temporary.path(), &repository);
    remove.args(["remote", "remove", SOURCE_REMOTE]);
    run_git(remove, "Git workflow source cleanup", cancelled)?;
    unset_partial_clone_extension(git, temporary.path(), &repository, cancelled)?;

    let mut inspect = repository_command(git, temporary.path(), &repository);
    inspect.args([
        "rev-parse",
        "--verify",
        &format!("{RELEASE_REF}^{{commit}}"),
    ]);
    let commit = run_git(inspect, "Git workflow commit inspection", cancelled)?;
    let commit = std::str::from_utf8(&commit)
        .context("Git workflow commit inspection returned non-UTF-8 output")?
        .trim();
    if !commit.eq_ignore_ascii_case(&release.advertised_object_id) {
        bail!("selected workflow release changed before it could be fetched");
    }
    ensure_not_cancelled(cancelled)?;

    Ok(FetchedWorkflowRelease {
        _temporary: temporary,
        repository: AbsolutePathBuf::from_absolute_path_checked(repository)
            .context("workflow Git staging path was not absolute")?,
        release: release.clone(),
    })
}

fn fetch_command(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    release: &ResolvedWorkflowRelease,
    blob_bytes: u64,
) -> Command {
    let selected_ref = release
        .tag
        .as_deref()
        .map_or("HEAD".to_string(), |tag| format!("refs/tags/{tag}"));
    let mut command = repository_command(git, working_directory, repository);
    command
        .args([
            "fetch",
            "--quiet",
            "--atomic",
            "--depth=1",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--no-write-commit-graph",
            "--refmap=",
        ])
        .arg(format!("--filter=blob:limit={blob_bytes}"))
        .arg(SOURCE_REMOTE)
        .arg(format!("+{selected_ref}:{RELEASE_REF}"));
    command
}

fn unset_partial_clone_extension(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut command = repository_command(git, working_directory, repository);
    command.args([
        "config",
        "--local",
        "--unset-all",
        "extensions.partialClone",
    ]);
    let (status, _, _, oversized) = crate::runner::run_bounded_command(
        command,
        super::git_command::GIT_COMMAND_TIMEOUT,
        super::git_command::MAX_GIT_OUTPUT_BYTES,
        cancelled,
    )
    .context("Git workflow source cleanup could not start or complete")?;
    if oversized {
        bail!("Git workflow source cleanup output exceeded its limit");
    }
    if status.success() || status.code() == Some(5) {
        return Ok(());
    }
    bail!("Git workflow source cleanup failed")
}

fn repository_command(git: &OsStr, working_directory: &Path, repository: &Path) -> Command {
    let mut command = super::git_command::trusted_git_command(git, working_directory);
    command
        .arg("--no-replace-objects")
        .arg("--git-dir")
        .arg(repository.join(".git"))
        .arg("--work-tree")
        .arg(repository)
        .env("GIT_NO_LAZY_FETCH", "1");
    command
}

fn run_git(
    command: Command,
    description: &str,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<Vec<u8>> {
    let (status, stdout, _, oversized) = crate::runner::run_bounded_command(
        command,
        FETCH_TIMEOUT,
        super::git_command::MAX_GIT_OUTPUT_BYTES,
        cancelled,
    )
    .with_context(|| format!("{description} could not start or complete"))?;
    if oversized {
        bail!("{description} output exceeded its limit");
    }
    if !status.success() {
        let status = status
            .code()
            .map_or_else(|| "terminated".to_string(), |code| code.to_string());
        bail!("{description} failed with exit status {status}");
    }
    Ok(stdout)
}

fn ensure_not_cancelled(cancelled: Option<&AtomicBool>) -> anyhow::Result<()> {
    if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
        bail!("workflow release fetch was cancelled");
    }
    Ok(())
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
