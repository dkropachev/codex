use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;

use super::ResolvedWorkflowRelease;
use super::WorkflowGitSource;

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
const GIT_CONFIG: &[&str] = &[
    "safe.bareRepository=explicit",
    "core.fsmonitor=false",
    "credential.helper=",
    "credential.interactive=never",
    "core.askPass=",
    "http.followRedirects=false",
    "protocol.allow=never",
    "protocol.file.allow=always",
    "protocol.https.allow=always",
    "protocol.ssh.allow=always",
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

fn resolve_workflow_git_release_with_git(
    git: &OsStr,
    source: &WorkflowGitSource,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    if cancelled.is_some_and(|cancelled| cancelled.load(std::sync::atomic::Ordering::Relaxed)) {
        bail!("workflow release check was cancelled");
    }
    let working_directory = tempfile::tempdir().context("failed to isolate Git release check")?;
    let command = ls_remote_command(git, source.as_os_str(), working_directory.path());
    let (status, stdout, stderr, stdout_oversized) = crate::runner::run_bounded_command(
        command,
        GIT_COMMAND_TIMEOUT,
        MAX_GIT_OUTPUT_BYTES,
        cancelled,
    )
    .context("Git release check could not start or complete")?;
    if stdout_oversized {
        bail!("Git release metadata exceeded {MAX_GIT_OUTPUT_BYTES} bytes");
    }
    if !status.success() {
        let status = status
            .code()
            .map_or_else(|| "terminated".to_string(), |code| code.to_string());
        let details = String::from_utf8_lossy(&stderr);
        bail!(
            "Git release check failed with exit status {status}: {}",
            details.trim()
        );
    }
    let output = std::str::from_utf8(&stdout).context("Git returned non-UTF-8 release metadata")?;
    super::release::resolve_workflow_release(output)
}

fn ls_remote_command(git: &OsStr, source: &OsStr, working_directory: &Path) -> Command {
    let mut command = Command::new(git);
    for config in GIT_CONFIG {
        command.args(["-c", config]);
    }
    command
        .arg("-c")
        .arg(format!("core.hooksPath={DISABLED_GIT_CONFIG_PATH}"))
        .args(["ls-remote", "--"])
        .arg(source)
        .args(["HEAD", "refs/tags/*"]);
    for (name, _) in std::env::vars_os() {
        if is_git_environment_variable(&name) {
            command.env_remove(name);
        }
    }
    command
        .current_dir(working_directory)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", working_directory)
        .env("GIT_CONFIG_GLOBAL", DISABLED_GIT_CONFIG_PATH)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", DISABLED_GIT_CONFIG_PATH)
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_DIR", working_directory.join("isolated.git"))
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

#[cfg(test)]
#[path = "git_command_tests.rs"]
mod tests;
