use crate::remote_commit::git_status_short;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::thread::sleep;
use std::time::Duration;

const GITHUB_RETRY_ATTEMPTS: usize = 3;
const GITHUB_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_COMMAND_OUTPUT_BYTES: usize = 12_000;
const MAX_STATUS_LINES: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    head_ref_name: String,
    head_repository: Option<GhRepository>,
    head_repository_owner: GhRepositoryOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GhRepository {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GhRepositoryOwner {
    login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RemoteRepoCiCheck {
    pub name: String,
    pub state: String,
    pub bucket: Option<String>,
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteRepoCiWorkflowOutcome {
    Skipped(String),
    Passed,
    Failed {
        watch_status: ExitStatus,
        checks: Vec<RemoteRepoCiCheck>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRepoCiWorkflow {
    pr: GhPr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteRepoCiWorkflowStart {
    Skipped(String),
    Ready(RemoteRepoCiWorkflow),
}

pub fn run_remote_workflow(cwd: &Path) -> Result<RemoteRepoCiWorkflowOutcome> {
    match start_remote_workflow(cwd)? {
        RemoteRepoCiWorkflowStart::Skipped(reason) => {
            Ok(RemoteRepoCiWorkflowOutcome::Skipped(reason))
        }
        RemoteRepoCiWorkflowStart::Ready(workflow) => run_started_remote_workflow(cwd, &workflow),
    }
}

pub fn start_remote_workflow(cwd: &Path) -> Result<RemoteRepoCiWorkflowStart> {
    ensure_gh_auth(cwd)?;
    gh_setup_git(cwd)?;
    let Some(pr) = current_pr(cwd)? else {
        return Ok(RemoteRepoCiWorkflowStart::Skipped(
            "Repo CI remote checks skipped because no PR is linked to the current branch."
                .to_string(),
        ));
    };
    Ok(RemoteRepoCiWorkflowStart::Ready(RemoteRepoCiWorkflow {
        pr,
    }))
}

pub fn run_started_remote_workflow(
    cwd: &Path,
    workflow: &RemoteRepoCiWorkflow,
) -> Result<RemoteRepoCiWorkflowOutcome> {
    ensure_clean_worktree(cwd)?;
    push_pr_head(cwd, &workflow.pr)?;
    let watch_output = run_gh_output_with_retry(
        cwd,
        &["pr", "checks", "--watch", "--fail-fast"],
        "watch GitHub PR checks",
    )?;
    if watch_output.status.success() {
        return Ok(RemoteRepoCiWorkflowOutcome::Passed);
    }
    let checks = pr_checks(cwd)?;
    Ok(RemoteRepoCiWorkflowOutcome::Failed {
        watch_status: watch_output.status,
        checks,
    })
}

pub fn watch_pr(cwd: &Path) -> Result<ExitStatus> {
    ensure_gh_auth(cwd)?;
    let output = run_gh_output_with_retry(
        cwd,
        &["pr", "checks", "--watch", "--fail-fast"],
        "watch GitHub PR checks",
    )?;
    Ok(output.status)
}

fn ensure_gh_auth(cwd: &Path) -> Result<()> {
    run_gh_output_with_retry(cwd, &["auth", "status"], "check GitHub CLI auth")?;
    Ok(())
}

fn gh_setup_git(cwd: &Path) -> Result<()> {
    run_gh_output_with_retry(cwd, &["auth", "setup-git"], "configure GitHub git auth")?;
    Ok(())
}

fn current_pr(cwd: &Path) -> Result<Option<GhPr>> {
    let args = [
        "pr",
        "view",
        "--json",
        "headRefName,headRepository,headRepositoryOwner",
    ];
    let command_display = format!("gh {}", args.join(" "));
    let mut last_error = None;

    for attempt in 1..=GITHUB_RETRY_ATTEMPTS {
        match Command::new("gh").args(args).current_dir(cwd).output() {
            Ok(output) if output.status.success() => {
                return Ok(Some(serde_json::from_slice(&output.stdout)?));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
                if combined.contains("no pull requests found")
                    || combined.contains("no pull request found")
                    || combined.contains("could not find pull request")
                {
                    return Ok(None);
                }
                last_error = Some(anyhow!(
                    "load GitHub PR metadata failed after attempt {attempt}/{GITHUB_RETRY_ATTEMPTS}: `{command_display}` exited with {}.\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    truncate_command_output(&output.stdout),
                    truncate_command_output(&output.stderr),
                ));
            }
            Err(err) => {
                last_error = Some(anyhow!(
                    "load GitHub PR metadata failed after attempt {attempt}/{GITHUB_RETRY_ATTEMPTS}: could not run `{command_display}`: {err}"
                ));
            }
        }

        if attempt < GITHUB_RETRY_ATTEMPTS {
            sleep(retry_delay(attempt));
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("retry loop did not run for `{command_display}`")))
}

fn push_pr_head(cwd: &Path, pr: &GhPr) -> Result<()> {
    let mut push_ref = String::from("HEAD:");
    push_ref.push_str(&pr.head_ref_name);
    let remote = if let Some(repo) = &pr.head_repository {
        format!(
            "git@github.com:{}/{}.git",
            pr.head_repository_owner.login, repo.name
        )
    } else {
        "origin".to_string()
    };
    run_command_with_retry(
        cwd,
        "git",
        &["push", &remote, &push_ref],
        "push PR head to GitHub",
    )?;
    Ok(())
}

fn pr_checks(cwd: &Path) -> Result<Vec<RemoteRepoCiCheck>> {
    let output = run_gh_output_with_retry(
        cwd,
        &["pr", "checks", "--json", "name,state,bucket,link"],
        "load GitHub PR checks",
    )?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn ensure_clean_worktree(cwd: &Path) -> Result<()> {
    let status_text = git_status_short(cwd)?;
    if status_text.trim().is_empty() {
        return Ok(());
    }
    let mut lines = status_text
        .lines()
        .take(MAX_STATUS_LINES)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let total_lines = status_text.lines().count();
    if total_lines > lines.len() {
        lines.push(format!("... and {} more", total_lines - lines.len()));
    }
    Err(anyhow!(
        "Repo CI remote checks require committed changes before pushing. The working tree is dirty:\n{}",
        lines.join("\n")
    ))
}

fn run_gh_output_with_retry(cwd: &Path, args: &[&str], action: &str) -> Result<Output> {
    run_command_with_retry(cwd, "gh", args, action)
}

fn run_command_with_retry(
    cwd: &Path,
    program: &str,
    args: &[&str],
    action: &str,
) -> Result<Output> {
    let command_display = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let mut last_error = None;

    for attempt in 1..=GITHUB_RETRY_ATTEMPTS {
        match Command::new(program).args(args).current_dir(cwd).output() {
            Ok(output) if output.status.success() => return Ok(output),
            Ok(output) => {
                last_error = Some(command_error(
                    &format!("{action} failed after attempt {attempt}/{GITHUB_RETRY_ATTEMPTS}"),
                    &command_display,
                    &output,
                ));
            }
            Err(err) => {
                last_error = Some(anyhow!(
                    "{action} failed after attempt {attempt}/{GITHUB_RETRY_ATTEMPTS}: could not run `{command_display}`: {err}"
                ));
            }
        }

        if attempt < GITHUB_RETRY_ATTEMPTS {
            sleep(retry_delay(attempt));
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("retry loop did not run for `{command_display}`")))
}

fn command_error(action: &str, command_display: &str, output: &Output) -> anyhow::Error {
    anyhow!(
        "{action}: `{command_display}` exited with {}.\nstdout:\n{}\nstderr:\n{}",
        output.status,
        truncate_command_output(&output.stdout),
        truncate_command_output(&output.stderr),
    )
}

fn retry_delay(attempt: usize) -> Duration {
    GITHUB_RETRY_BASE_DELAY.saturating_mul(2_u32.pow((attempt - 1) as u32))
}

fn truncate_command_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() <= MAX_COMMAND_OUTPUT_BYTES {
        return text.to_string();
    }
    let keep = MAX_COMMAND_OUTPUT_BYTES / 2;
    let head_end = floor_char_boundary(&text, keep);
    let tail_start = ceil_char_boundary(&text, text.len().saturating_sub(keep));
    format!("{}\n...\n{}", &text[..head_end], &text[tail_start..])
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dirty_worktree_blocks_remote_workflow() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        fs::create_dir(&repo_root).expect("create repo");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_root)
            .output()
            .expect("git init");
        fs::write(repo_root.join("dirty.txt"), "dirty").expect("write dirty");

        let err = ensure_clean_worktree(&repo_root).expect_err("dirty tree should fail");
        assert!(err.to_string().contains("working tree is dirty"));
        assert!(err.to_string().contains("dirty.txt"));
    }
}
