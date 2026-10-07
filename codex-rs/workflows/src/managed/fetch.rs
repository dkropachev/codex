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

mod checkout;
mod tree;

pub(super) use checkout::StagedWorkflowRelease;
pub(super) use tree::portable_path;

const RELEASE_REF: &str = "refs/codex/workflow-release";
const SOURCE_REMOTE: &str = "codex-workflow-source";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
pub(in crate::managed) struct VerificationLimits {
    blob_bytes: u64,
    object_entries: usize,
    object_bytes: u64,
    worktree_entries: usize,
    staging_entries: usize,
    staging_bytes: u64,
    pub(in crate::managed) post_install_entries: usize,
    pub(in crate::managed) post_install_bytes: u64,
}

pub(super) const VERIFICATION_LIMITS: VerificationLimits = VerificationLimits {
    blob_bytes: 128 * 1024 * 1024,
    object_entries: 16_384, // Keeps `cat-file` metadata below the 1 MiB output cap.
    object_bytes: 256 * 1024 * 1024,
    worktree_entries: 8_192,
    staging_entries: 200_000,
    staging_bytes: 256 * 1024 * 1024,
    post_install_entries: 250_000,
    post_install_bytes: 2 * 1024 * 1024 * 1024,
};
#[allow(dead_code, reason = "used by the managed installation stage")]
pub(super) struct FetchedWorkflowRelease {
    temporary: tempfile::TempDir,
    pub(super) repository: AbsolutePathBuf,
    pub(super) release: ResolvedWorkflowRelease,
}

#[allow(dead_code, reason = "used by the managed installation stage")]
pub(super) fn stage_resolved_workflow_release_cancellable(
    staging_root: &AbsolutePathBuf,
    source: &WorkflowGitSource,
    release: &ResolvedWorkflowRelease,
    cancelled: &AtomicBool,
) -> anyhow::Result<StagedWorkflowRelease> {
    let fetched = fetch_with_options(
        OsStr::new("git"),
        staging_root,
        source,
        release,
        Some(cancelled),
        VERIFICATION_LIMITS,
    )?;
    checkout::checkout_fetched_release(
        OsStr::new("git"),
        fetched,
        VERIFICATION_LIMITS,
        Some(cancelled),
    )
}

fn fetch_with_options(
    git: &OsStr,
    staging_root: &AbsolutePathBuf,
    source: &WorkflowGitSource,
    release: &ResolvedWorkflowRelease,
    cancelled: Option<&AtomicBool>,
    limits: VerificationLimits,
) -> anyhow::Result<FetchedWorkflowRelease> {
    ensure_not_cancelled(cancelled)?;
    release.validate_identity()?;
    fs::create_dir_all(staging_root).context("failed to create workflow staging root")?;
    let staging_metadata = fs::symlink_metadata(staging_root)?;
    if !staging_metadata.is_dir()
        || staging_metadata.file_type().is_symlink()
        || is_windows_reparse_point(&staging_metadata)
    {
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
            limits
                .blob_bytes
                .checked_add(1)
                .context("workflow blob limit overflow")?,
        ),
        "Git workflow release fetch",
        cancelled,
    )?;

    let mut remove = repository_command(git, temporary.path(), &repository);
    remove.args(["remote", "remove", SOURCE_REMOTE]);
    run_git(remove, "Git workflow source cleanup", cancelled)?;
    unset_partial_clone_extension(git, temporary.path(), &repository, cancelled)?;
    verify_isolated(git, temporary.path(), &repository, cancelled)?;

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
    verify_objects(
        git,
        temporary.path(),
        &repository,
        commit,
        limits,
        cancelled,
    )?;
    tree::verify_tracked_tree(git, temporary.path(), &repository, commit, cancelled)?;
    inspect_staging(temporary.path(), limits, cancelled)?;
    ensure_not_cancelled(cancelled)?;

    Ok(FetchedWorkflowRelease {
        temporary,
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

fn verify_isolated(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut config = repository_command(git, working_directory, repository);
    config.args(["config", "--local", "--null", "--name-only", "--list"]);
    let config = run_git(config, "Git workflow configuration inspection", cancelled)?;
    for key in config
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key)
            .context("Git workflow configuration contains a non-UTF-8 key")?
            .to_ascii_lowercase();
        if key.starts_with("remote.")
            || key.starts_with("include.")
            || key.starts_with("includeif.")
            || matches!(
                key.as_str(),
                "extensions.partialclone" | "extensions.worktreeconfig"
            )
        {
            bail!("fetched workflow repository retained remote Git configuration");
        }
    }
    for relative in ["objects/info/alternates", "objects/info/http-alternates"] {
        let path = repository.join(".git").join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => bail!("fetched workflow repository retained alternate Git objects"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("failed to inspect Git object alternates"),
        }
    }

    let mut refs = repository_command(git, working_directory, repository);
    refs.args(["for-each-ref", "--format=%(refname)"]);
    let refs = run_git(refs, "Git workflow reference inspection", cancelled)?;
    if std::str::from_utf8(&refs)
        .context("Git workflow reference inspection returned non-UTF-8 output")?
        .lines()
        .collect::<Vec<_>>()
        != [RELEASE_REF]
    {
        bail!("fetched workflow repository contains unexpected Git references");
    }
    Ok(())
}

fn verify_objects(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    commit: &str,
    limits: VerificationLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut complete = repository_command(git, working_directory, repository);
    complete
        .args([
            "rev-list",
            "--quiet",
            "--objects",
            "--missing=error",
            "--no-object-names",
        ])
        .arg(commit)
        .arg("--");
    run_git(complete, "Git workflow object verification", cancelled)?;

    let mut inspect = repository_command(git, working_directory, repository);
    inspect.args([
        "cat-file",
        "--batch-check=%(objecttype) %(objectsize)",
        "--batch-all-objects",
    ]);
    let output = run_git(inspect, "Git workflow object inspection", cancelled)?;
    validate_object_listing(&output, limits, cancelled)
}

fn validate_object_listing(
    output: &[u8],
    limits: VerificationLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        ensure_not_cancelled(cancelled)?;
        let mut fields = line.split(|byte| *byte == b' ');
        let (Some(kind), Some(size), None) = (fields.next(), fields.next(), fields.next()) else {
            bail!("Git returned malformed workflow object metadata");
        };
        if !matches!(kind, b"blob" | b"tree" | b"commit" | b"tag") {
            bail!("Git returned an unsupported workflow object type");
        }
        let size = std::str::from_utf8(size)
            .context("Git returned a non-UTF-8 workflow object size")?
            .parse::<u64>()
            .context("Git returned an invalid workflow object size")?;
        if kind == b"blob" && size > limits.blob_bytes {
            bail!("workflow Git blob exceeds {} bytes", limits.blob_bytes);
        }
        entries += 1;
        if entries > limits.object_entries {
            bail!("workflow object count exceeds {}", limits.object_entries);
        }
        bytes = bytes
            .checked_add(size)
            .context("workflow Git object size overflow")?;
        if bytes > limits.object_bytes {
            bail!(
                "workflow Git object set exceeds {} bytes",
                limits.object_bytes
            );
        }
    }
    if entries == 0 {
        bail!("Git returned no workflow object metadata");
    }
    Ok(())
}

fn inspect_staging(
    root: &Path,
    limits: VerificationLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            ensure_not_cancelled(cancelled)?;
            let entry = entry?;
            entries += 1;
            if entries > limits.staging_entries {
                bail!(
                    "workflow Git staging exceeds {} entries",
                    limits.staging_entries
                );
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) {
                bail!("workflow Git staging contains a symbolic link or reparse point");
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .context("workflow Git staging size overflow")?;
                if bytes > limits.staging_bytes {
                    bail!(
                        "workflow Git staging exceeds {} bytes",
                        limits.staging_bytes
                    );
                }
            } else {
                bail!("workflow Git staging contains a special file");
            }
        }
    }
    Ok(())
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

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
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
