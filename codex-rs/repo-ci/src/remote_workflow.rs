use crate::remote_commit::RemoteCommitApplied;
use crate::remote_commit::RemoteCommitDecision;
use crate::remote_commit::apply_remote_commit_decision;
use crate::remote_commit::fallback_remote_commit_decision;
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
pub struct RemoteRepoCiWorkflowRun {
    pub outcome: RemoteRepoCiWorkflowOutcome,
    pub prepared_commit: Option<RemoteCommitApplied>,
    pub pushed_head: Option<String>,
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
    Ok(run_started_remote_workflow_with_commit_decision(cwd, workflow, None)?.outcome)
}

pub fn run_started_remote_workflow_with_commit_decision(
    cwd: &Path,
    workflow: &RemoteRepoCiWorkflow,
    commit_decision: Option<&RemoteCommitDecision>,
) -> Result<RemoteRepoCiWorkflowRun> {
    let prepared_commit = prepare_remote_commit(cwd, commit_decision)?;
    ensure_clean_worktree(cwd)?;
    let pushed_head = local_head(cwd)?;
    push_pr_head(cwd, &workflow.pr)?;
    ensure_remote_ref_matches_head(cwd, &workflow.pr, &pushed_head)?;
    let watch_output = run_gh_output_with_retry(
        cwd,
        &["pr", "checks", "--watch", "--fail-fast"],
        "watch GitHub PR checks",
    )?;
    if watch_output.status.success() {
        ensure_clean_worktree(cwd)?;
        ensure_local_head_matches(cwd, &pushed_head)?;
        ensure_remote_ref_matches_head(cwd, &workflow.pr, &pushed_head)?;
        return Ok(RemoteRepoCiWorkflowRun {
            outcome: RemoteRepoCiWorkflowOutcome::Passed,
            prepared_commit,
            pushed_head: Some(pushed_head),
        });
    }
    let checks = pr_checks(cwd)?;
    Ok(RemoteRepoCiWorkflowRun {
        outcome: RemoteRepoCiWorkflowOutcome::Failed {
            watch_status: watch_output.status,
            checks,
        },
        prepared_commit,
        pushed_head: Some(pushed_head),
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
    let remote = remote_target(cwd, pr);
    let mut push_ref = String::from("HEAD:");
    push_ref.push_str(&pr.head_ref_name);
    run_command_with_retry(
        cwd,
        "git",
        &["push", &remote, &push_ref],
        "push PR head to GitHub",
    )?;
    Ok(())
}

fn remote_target(cwd: &Path, pr: &GhPr) -> String {
    if let Some(repo) = &pr.head_repository {
        if let Some(remote) =
            matching_github_remote(cwd, &pr.head_repository_owner.login, &repo.name)
        {
            return remote;
        }
        let protocol = gh_git_protocol(cwd);
        github_remote_url(
            &pr.head_repository_owner.login,
            &repo.name,
            protocol.as_deref(),
        )
    } else {
        "origin".to_string()
    }
}

fn matching_github_remote(cwd: &Path, owner: &str, repo: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "-v"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?;
            let url = parts.next()?;
            github_remote_matches(url, owner, repo).then(|| name.to_string())
        })
        .next()
}

fn github_remote_url(owner: &str, repo: &str, protocol: Option<&str>) -> String {
    if protocol == Some("https") {
        format!("https://github.com/{owner}/{repo}.git")
    } else {
        format!("git@github.com:{owner}/{repo}.git")
    }
}

fn gh_git_protocol(cwd: &Path) -> Option<String> {
    let output = Command::new("gh")
        .args(["config", "get", "git_protocol"])
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn github_remote_matches(url: &str, owner: &str, repo: &str) -> bool {
    normalize_github_remote(url)
        .as_deref()
        .is_some_and(|remote| remote.eq_ignore_ascii_case(&format!("{owner}/{repo}")))
}

fn normalize_github_remote(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("git://github.com/"))
    {
        return Some(rest.trim_matches('/').to_string());
    }
    url.strip_prefix("git@github.com:")
        .map(|rest| rest.trim_matches('/').to_string())
}

fn remote_head_ref(pr: &GhPr) -> String {
    let mut head_ref = String::from("refs/heads/");
    head_ref.push_str(&pr.head_ref_name);
    head_ref
}

fn prepare_remote_commit(
    cwd: &Path,
    commit_decision: Option<&RemoteCommitDecision>,
) -> Result<Option<RemoteCommitApplied>> {
    let fallback;
    let decision = match commit_decision {
        Some(decision) => decision,
        None => {
            fallback = fallback_remote_commit_decision();
            &fallback
        }
    };
    apply_remote_commit_decision(cwd, decision)
}

fn local_head(cwd: &Path) -> Result<String> {
    let output = run_command_with_retry(cwd, "git", &["rev-parse", "HEAD"], "resolve local HEAD")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn ensure_local_head_matches(cwd: &Path, expected: &str) -> Result<()> {
    let actual = local_head(cwd)?;
    if actual == expected {
        return Ok(());
    }
    Err(anyhow!(
        "Repo CI remote checks cannot be marked passed because local HEAD changed after push: expected {expected}, found {actual}"
    ))
}

fn remote_head(cwd: &Path, pr: &GhPr) -> Result<Option<String>> {
    let remote = remote_target(cwd, pr);
    let head_ref = remote_head_ref(pr);
    let output = run_command_with_retry(
        cwd,
        "git",
        &["ls-remote", &remote, &head_ref],
        "resolve pushed PR head",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.split_whitespace().next().map(str::to_string)))
}

fn ensure_remote_ref_matches_head(cwd: &Path, pr: &GhPr, expected: &str) -> Result<()> {
    let actual = remote_head(cwd, pr)?.ok_or_else(|| {
        anyhow!(
            "Repo CI remote checks cannot be marked passed because {} was not found on {}",
            remote_head_ref(pr),
            remote_target(cwd, pr)
        )
    })?;
    if actual == expected {
        return Ok(());
    }
    Err(anyhow!(
        "Repo CI remote checks cannot be marked passed because pushed PR head does not match local HEAD: expected {expected}, found {actual}"
    ))
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
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;

    fn init_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        fs::create_dir(&repo_root).expect("create repo");
        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.email", "repo-ci@example.com"]);
        run_git(&repo_root, &["config", "user.name", "Repo CI"]);
        fs::write(repo_root.join("base.txt"), "base\n").expect("write base");
        run_git(&repo_root, &["add", "base.txt"]);
        run_git(&repo_root, &["commit", "-m", "base"]);
        (temp, repo_root)
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed with {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn local_origin_pr(head_ref_name: &str) -> GhPr {
        GhPr {
            head_ref_name: head_ref_name.to_string(),
            head_repository: None,
            head_repository_owner: GhRepositoryOwner {
                login: "owner".to_string(),
            },
        }
    }

    fn github_pr(owner: &str, repo: &str, head_ref_name: &str) -> GhPr {
        GhPr {
            head_ref_name: head_ref_name.to_string(),
            head_repository: Some(GhRepository {
                name: repo.to_string(),
            }),
            head_repository_owner: GhRepositoryOwner {
                login: owner.to_string(),
            },
        }
    }

    #[test]
    fn remote_target_prefers_matching_existing_https_remote() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo_root = temp.path().join("repo");
        fs::create_dir(&repo_root).expect("create repo");
        run_git(&repo_root, &["init"]);
        run_git(
            &repo_root,
            &["remote", "add", "fork", "https://github.com/owner/repo.git"],
        );
        let pr = github_pr("owner", "repo", "feature");

        assert_eq!(remote_target(&repo_root, &pr), "fork");
    }

    #[test]
    fn github_remote_url_uses_https_when_gh_protocol_is_https() {
        assert_eq!(
            github_remote_url("owner", "repo", Some("https")),
            "https://github.com/owner/repo.git"
        );
        assert_eq!(
            github_remote_url("owner", "repo", Some("ssh")),
            "git@github.com:owner/repo.git"
        );
    }

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

    #[test]
    fn remote_workflow_prepares_dirty_tree_with_fallback_commit() {
        let (_temp, repo_root) = init_repo();
        fs::write(repo_root.join("base.txt"), "base\nchanged\n").expect("write change");

        let applied = prepare_remote_commit(&repo_root, None)
            .expect("prepare commit")
            .expect("dirty tree should be committed");

        assert_eq!(
            applied,
            RemoteCommitApplied {
                strategy: crate::RemoteCommitStrategy::SeparateCommit,
                title: Some("repo-ci: prepare remote retry".to_string()),
            }
        );
        assert!(
            git_status_short(&repo_root)
                .expect("status")
                .trim()
                .is_empty()
        );
        let log = Command::new("git")
            .args(["log", "--oneline", "-1"])
            .current_dir(&repo_root)
            .output()
            .expect("git log");
        assert!(String::from_utf8_lossy(&log.stdout).contains("repo-ci: prepare remote retry"));
    }

    #[test]
    fn remote_head_validation_rejects_unpushed_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        let remote_root = temp.path().join("origin.git");
        fs::create_dir(&remote_root).expect("create remote dir");
        run_git(&remote_root, &["init", "--bare"]);

        let repo_root = temp.path().join("repo");
        fs::create_dir(&repo_root).expect("create repo");
        run_git(&repo_root, &["init"]);
        run_git(&repo_root, &["config", "user.email", "repo-ci@example.com"]);
        run_git(&repo_root, &["config", "user.name", "Repo CI"]);
        fs::write(repo_root.join("base.txt"), "base\n").expect("write base");
        run_git(&repo_root, &["add", "base.txt"]);
        run_git(&repo_root, &["commit", "-m", "base"]);
        run_git(
            &repo_root,
            &["remote", "add", "origin", remote_root.to_str().unwrap()],
        );
        run_git(&repo_root, &["push", "origin", "HEAD:master"]);

        fs::write(repo_root.join("base.txt"), "base\nlocal\n").expect("write local change");
        run_git(&repo_root, &["commit", "-am", "local"]);
        let local = local_head(&repo_root).expect("local head");
        let pr = local_origin_pr("master");

        let err = ensure_remote_ref_matches_head(&repo_root, &pr, &local)
            .expect_err("unpushed local head should not validate");

        assert!(err.to_string().contains("does not match local HEAD"));
    }
}
