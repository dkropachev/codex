use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;

use super::ResolvedWorkflowRelease;
use super::WorkflowGitSource;

pub(super) const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
const GIT_CONFIG: &[&str] = &[
    "safe.bareRepository=explicit",
    "core.fsmonitor=false",
    "credential.helper=",
    "credential.interactive=never",
    "fetch.fsckObjects=true",
    "fetch.writeCommitGraph=false",
    "core.askPass=",
    "http.followRedirects=false",
    "protocol.allow=never",
    "protocol.file.allow=always",
    "protocol.https.allow=always",
    "protocol.ssh.allow=always",
    "submodule.recurse=false",
    "transfer.fsckObjects=true",
];

#[cfg(windows)]
const DISABLED_GIT_CONFIG_PATH: &str = "NUL";
#[cfg(not(windows))]
const DISABLED_GIT_CONFIG_PATH: &str = "/dev/null";

pub(super) fn resolve_workflow_git_release(
    source: &WorkflowGitSource,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    resolve_workflow_git_release_with_git(OsStr::new("git"), source, cancelled)
}

/// Resolves one previously installed tag, including annotated-tag peeling.
pub(super) fn resolve_installed_workflow_tag(
    source: &WorkflowGitSource,
    tag: &str,
    cancelled: &AtomicBool,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    let working_directory = tempfile::tempdir().context("failed to isolate Git tag check")?;
    let mut command = trusted_git_command(OsStr::new("git"), working_directory.path());
    command
        .args(["ls-remote", "--"])
        .arg(source.as_os_str())
        .arg(format!("refs/tags/{tag}"))
        .arg(format!("refs/tags/{tag}^{{}}"))
        .env("GIT_DIR", working_directory.path().join("isolated.git"));
    let (status, stdout, _, oversized) = crate::runner::run_bounded_command(
        command,
        GIT_COMMAND_TIMEOUT,
        MAX_GIT_OUTPUT_BYTES,
        Some(cancelled),
    )
    .context("installed Git tag check could not start or complete")?;
    if oversized {
        bail!("installed Git tag metadata exceeded its limit");
    }
    if !status.success() {
        bail!("installed Git tag check failed with status {status}");
    }
    if stdout.is_empty() {
        bail!("installed managed workflow release tag disappeared");
    }
    let output =
        std::str::from_utf8(&stdout).context("installed Git tag metadata was not UTF-8")?;
    let release = super::release::resolve_workflow_release(output)?;
    if release.tag.as_deref() != Some(tag) {
        bail!("installed Git tag check returned an unexpected release");
    }
    Ok(release)
}

fn resolve_workflow_git_release_with_git(
    git: &OsStr,
    source: &WorkflowGitSource,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    resolve_workflow_git_release_with_options(
        git,
        source,
        cancelled,
        GIT_COMMAND_TIMEOUT,
        MAX_GIT_OUTPUT_BYTES,
    )
}

fn resolve_workflow_git_release_with_options(
    git: &OsStr,
    source: &WorkflowGitSource,
    cancelled: Option<&AtomicBool>,
    timeout: Duration,
    maximum_stdout_bytes: usize,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    if cancelled.is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Relaxed)) {
        bail!("workflow release check was cancelled");
    }
    let working_directory = tempfile::tempdir().context("failed to isolate Git release check")?;
    let command = ls_remote_command(git, source.as_os_str(), working_directory.path());
    let (status, stdout, _stderr, stdout_oversized) =
        crate::runner::run_bounded_command(command, timeout, maximum_stdout_bytes, cancelled)
            .context("Git release check could not start or complete")?;
    if stdout_oversized {
        bail!("Git release metadata exceeded {maximum_stdout_bytes} bytes");
    }
    if !status.success() {
        let status = status
            .code()
            .map_or_else(|| "terminated".to_string(), |code| code.to_string());
        bail!("Git release check failed with exit status {status}");
    }
    let output = std::str::from_utf8(&stdout).context("Git returned non-UTF-8 release metadata")?;
    let release = super::release::resolve_workflow_release(output)?;
    release.validate_identity()?;
    Ok(release)
}

fn ls_remote_command(git: &OsStr, source: &OsStr, working_directory: &Path) -> Command {
    let mut command = trusted_git_command(git, working_directory);
    command
        .args(["ls-remote", "--"])
        .arg(source)
        .args(["HEAD", "refs/tags/*"])
        .env("GIT_DIR", working_directory.join("isolated.git"));
    command
}

pub(super) fn trusted_git_command(git: &OsStr, working_directory: &Path) -> Command {
    let mut command = Command::new(git);
    for config in GIT_CONFIG {
        command.args(["-c", config]);
    }
    command
        .arg("-c")
        .arg(format!("core.hooksPath={DISABLED_GIT_CONFIG_PATH}"));
    remove_git_environment_variables(&mut command, std::env::vars_os().map(|(name, _)| name));
    command
        .current_dir(working_directory)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", working_directory)
        .env("GIT_CONFIG_GLOBAL", DISABLED_GIT_CONFIG_PATH)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", DISABLED_GIT_CONFIG_PATH)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C");
    codex_protocol::shell_environment::scrub_non_inheritable_env_vars(&mut command);
    command
}

fn is_git_environment_variable(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        let name = name.to_ascii_uppercase();
        name.starts_with("GIT_") || name.starts_with("SSH_ASKPASS")
    })
}

fn remove_git_environment_variables(
    command: &mut Command,
    names: impl IntoIterator<Item = impl AsRef<OsStr>>,
) {
    for name in names {
        if is_git_environment_variable(name.as_ref()) {
            command.env_remove(name.as_ref());
        }
    }
}

#[cfg(test)]
#[path = "git_command_tests.rs"]
mod tests;
