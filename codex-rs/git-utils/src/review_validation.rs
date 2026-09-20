//! Executor-backed Git checks used to resolve and validate review targets.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;

use crate::CommitLogEntry;
use crate::ReviewCommandOutput;
use crate::ReviewCommandRunner;
use crate::pull_request::REVIEW_COMMAND_OUTPUT_BYTES_CAP;
use crate::pull_request::run_git;

pub(crate) const REVIEW_SCOPE_COMMIT_LIMIT: usize = 100;
const EXECUTABLE_FILTER_CONFIG_PATTERN: &str = r"^filter\..*\.(clean|process)$";

/// Returns whether the selected repository has staged, unstaged, or untracked changes.
pub async fn has_uncommitted_changes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<bool> {
    let disabled_hooks_path = match repository_root.infer_path_convention() {
        Some(PathConvention::Posix) => "/dev/null",
        Some(PathConvention::Windows) => "NUL",
        None => bail!("could not determine the review repository path convention"),
    };
    let filter_config = run_git(
        runner,
        repository_root,
        [
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            EXECUTABLE_FILTER_CONFIG_PATTERN,
        ],
    )
    .await
    .context("failed to inspect executable Git filters")?;
    if !matches!(filter_config.exit_code, 0 | 1) {
        return Err(command_error("git config", &filter_config));
    }
    if filter_config.stdout.len() >= REVIEW_COMMAND_OUTPUT_BYTES_CAP {
        bail!("`git config` output exceeded the review command limit");
    }
    let mut drivers = filter_config
        .stdout
        .split('\0')
        .filter_map(|key| {
            key.strip_suffix(".clean")
                .or_else(|| key.strip_suffix(".process"))
        })
        .collect::<Vec<_>>();
    drivers.sort_unstable();
    drivers.dedup();
    if drivers
        .iter()
        .any(|driver| driver.contains(['=', '\u{fffd}']))
    {
        bail!("Git filter name cannot be overridden safely");
    }

    let mut argv = vec![
        "git".to_string(),
        "-c".to_string(),
        format!("core.hooksPath={disabled_hooks_path}"),
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
    ];
    for driver in drivers {
        argv.extend([
            "-c".to_string(),
            format!("{driver}.clean="),
            "-c".to_string(),
            format!("{driver}.process="),
            "-c".to_string(),
            format!("{driver}.required=false"),
        ]);
    }
    argv.extend([
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "--untracked-files=all".to_string(),
        "--ignore-submodules=dirty".to_string(),
    ]);
    let command = crate::ReviewCommand::new(argv, repository_root.clone())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    let output = runner
        .run(command)
        .await
        .context("failed to inspect uncommitted review changes")?;
    require_success("git status", &output)?;
    Ok(!output.stdout.trim().is_empty())
}

pub(crate) async fn recent_review_commits(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<Vec<CommitLogEntry>> {
    let format = "--pretty=format:%H%x1f%ct%x1f%s";
    let output = run_git(
        runner,
        repository_root,
        ["-c", "log.showSignature=false", "log", "-n", "100", format],
    )
    .await
    .context("failed to list recent review commits")?;
    require_success("git log", &output)?;

    output
        .stdout
        .lines()
        .filter(|line| !line.is_empty())
        .take(REVIEW_SCOPE_COMMIT_LIMIT)
        .map(parse_commit_log_entry)
        .collect()
}

fn require_success(command: &str, output: &ReviewCommandOutput) -> Result<()> {
    if output.success() {
        Ok(())
    } else {
        Err(command_error(command, output))
    }
}

fn command_error(command: &str, output: &ReviewCommandOutput) -> anyhow::Error {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        anyhow::anyhow!("`{command}` exited with {}", output.exit_code)
    } else {
        anyhow::anyhow!("`{command}` exited with {}: {stderr}", output.exit_code)
    }
}

fn parse_commit_log_entry(line: &str) -> Result<CommitLogEntry> {
    let mut fields = line.splitn(/*n*/ 3, '\u{001f}');
    let sha = fields.next().unwrap_or_default().trim();
    let timestamp = fields.next().unwrap_or_default().trim();
    let subject = fields.next().unwrap_or_default().trim();
    if sha.is_empty() || timestamp.is_empty() {
        bail!("`git log` returned a malformed commit summary");
    }
    let timestamp = timestamp
        .parse::<i64>()
        .context("`git log` returned an invalid commit timestamp")?;
    Ok(CommitLogEntry {
        sha: sha.to_string(),
        timestamp,
        subject: subject.to_string(),
    })
}

#[cfg(test)]
#[path = "review_validation_tests.rs"]
mod tests;
