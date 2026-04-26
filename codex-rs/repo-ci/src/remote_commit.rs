use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::path::Path;
use std::process::Command;
use std::process::Output;

const MAX_COMMAND_OUTPUT_BYTES: usize = 12_000;
const REMOTE_COMMIT_DECISION_MAX_DIFF_BYTES: usize = 16_000;
const FALLBACK_COMMIT_TITLE: &str = "repo-ci: prepare remote retry";
const FALLBACK_COMMIT_BODY: &str =
    "Created automatically so repo-ci can push the current changes before remote checks.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommitDecisionContext {
    pub changed_paths: Vec<String>,
    pub status_short: String,
    pub recent_commits: String,
    pub change_details: RemoteCommitChangeDetails,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCommitChangeDetails {
    Diff { diff: String },
    PathsOnly { diff_bytes: usize, max_bytes: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCommitDecision {
    pub strategy: RemoteCommitStrategy,
    pub title: String,
    pub body: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteCommitStrategy {
    AmendPriorCommit,
    SeparateCommit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCommitApplied {
    pub strategy: RemoteCommitStrategy,
    pub title: Option<String>,
}

pub fn remote_commit_decision_context(cwd: &Path) -> Result<Option<RemoteCommitDecisionContext>> {
    let repo_root = crate::repo_root_for_cwd(cwd)?;
    remote_commit_decision_context_for_root(&repo_root)
}

fn remote_commit_decision_context_for_root(
    repo_root: &Path,
) -> Result<Option<RemoteCommitDecisionContext>> {
    let status_short = git_status_short(repo_root)?;
    if status_short.trim().is_empty() {
        return Ok(None);
    }
    let changed_paths = changed_paths_from_status(&status_short);
    let recent_commits = recent_commits(repo_root)?;
    let change_details = commit_change_details(repo_root)?;
    Ok(Some(RemoteCommitDecisionContext {
        changed_paths,
        status_short,
        recent_commits,
        change_details,
    }))
}

pub fn render_remote_commit_decision_prompt(context: &RemoteCommitDecisionContext) -> String {
    let change_details = match &context.change_details {
        RemoteCommitChangeDetails::Diff { diff } => {
            format!("Diff:\n```diff\n{diff}\n```")
        }
        RemoteCommitChangeDetails::PathsOnly {
            diff_bytes,
            max_bytes,
        } => format!(
            "Diff omitted because it is {diff_bytes} bytes, above the {max_bytes} byte limit. Use only the changed paths and status below."
        ),
    };
    format!(
        "Decide how repo-ci should commit the current uncommitted changes before pushing remote CI.\n\
Return strict JSON only.\n\n\
Rules:\n\
- Choose `amendPriorCommit` only when the changes are clearly a fixup or continuation of the latest commit.\n\
- Choose `separateCommit` when the changes are independent, ambiguous, or too large to evaluate confidently.\n\
- If you choose `separateCommit`, provide a concise commit title and body.\n\
- If the diff is omitted, rely only on the changed file paths, status, and recent commits.\n\
- Do not ask questions and do not describe shell commands.\n\n\
Recent commits:\n```text\n{}\n```\n\n\
Changed paths:\n```text\n{}\n```\n\n\
Git status:\n```text\n{}\n```\n\n\
{change_details}",
        if context.recent_commits.trim().is_empty() {
            "(no commits found)"
        } else {
            context.recent_commits.trim()
        },
        if context.changed_paths.is_empty() {
            "(no changed paths recorded)".to_string()
        } else {
            context.changed_paths.join("\n")
        },
        context.status_short.trim(),
    )
}

pub fn remote_commit_decision_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "strategy": {
                "type": "string",
                "enum": ["amendPriorCommit", "separateCommit"]
            },
            "title": { "type": "string" },
            "body": { "type": "string" },
            "rationale": { "type": "string" }
        },
        "required": ["strategy", "title", "body", "rationale"]
    })
}

pub fn fallback_remote_commit_decision() -> RemoteCommitDecision {
    RemoteCommitDecision {
        strategy: RemoteCommitStrategy::SeparateCommit,
        title: FALLBACK_COMMIT_TITLE.to_string(),
        body: FALLBACK_COMMIT_BODY.to_string(),
        rationale: "The commit decision agent did not produce a usable decision.".to_string(),
    }
}

pub fn apply_remote_commit_decision(
    cwd: &Path,
    decision: &RemoteCommitDecision,
) -> Result<Option<RemoteCommitApplied>> {
    let repo_root = crate::repo_root_for_cwd(cwd)?;
    if remote_commit_decision_context_for_root(&repo_root)?.is_none() {
        return Ok(None);
    }
    run_git_output(
        &repo_root,
        &["add", "--all"],
        "stage repo-ci remote changes",
    )?;
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet", "--exit-code"])
        .current_dir(&repo_root)
        .status()
        .context("failed to inspect staged repo-ci changes")?;
    if staged.success() {
        return Ok(None);
    }

    match decision.strategy {
        RemoteCommitStrategy::AmendPriorCommit => {
            run_git_output(
                &repo_root,
                &["commit", "--amend", "--no-edit"],
                "amend prior commit for repo-ci remote checks",
            )?;
            Ok(Some(RemoteCommitApplied {
                strategy: RemoteCommitStrategy::AmendPriorCommit,
                title: None,
            }))
        }
        RemoteCommitStrategy::SeparateCommit => {
            let fallback = fallback_remote_commit_decision();
            let title = non_empty_trimmed(&decision.title).unwrap_or(fallback.title.as_str());
            let body = non_empty_trimmed(&decision.body).unwrap_or(fallback.body.as_str());
            let mut command = Command::new("git");
            command
                .arg("commit")
                .arg("-m")
                .arg(title)
                .arg("-m")
                .arg(body)
                .current_dir(&repo_root);
            let output = command
                .output()
                .context("failed to create repo-ci remote commit")?;
            if !output.status.success() {
                return Err(command_error(
                    "create repo-ci remote commit",
                    "git commit -m <title> -m <body>",
                    &output,
                ));
            }
            Ok(Some(RemoteCommitApplied {
                strategy: RemoteCommitStrategy::SeparateCommit,
                title: Some(title.to_string()),
            }))
        }
    }
}

pub(crate) fn git_status_short(cwd: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(cwd)
        .output()
        .context("failed to inspect git worktree state")?;
    if !output.status.success() {
        return Err(command_error(
            "inspect git worktree state",
            "git status --short --untracked-files=all",
            &output,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn changed_paths_from_status(status_short: &str) -> Vec<String> {
    status_short
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|path| {
            path.rsplit(" -> ")
                .next()
                .unwrap_or(path)
                .trim_matches('"')
                .to_string()
        })
        .collect()
}

fn recent_commits(cwd: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["log", "--oneline", "--decorate", "-5"])
        .current_dir(cwd)
        .output()
        .context("failed to inspect recent commits")?;
    if !output.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn commit_change_details(cwd: &Path) -> Result<RemoteCommitChangeDetails> {
    let mut diff = Vec::new();
    append_diff_command(
        cwd,
        &["diff", "--cached", "--no-ext-diff", "--binary"],
        &mut diff,
    )?;
    append_diff_command(cwd, &["diff", "--no-ext-diff", "--binary"], &mut diff)?;
    for path in untracked_paths(cwd)? {
        append_untracked_diff(cwd, &path, &mut diff)?;
    }
    if diff.len() > REMOTE_COMMIT_DECISION_MAX_DIFF_BYTES {
        return Ok(RemoteCommitChangeDetails::PathsOnly {
            diff_bytes: diff.len(),
            max_bytes: REMOTE_COMMIT_DECISION_MAX_DIFF_BYTES,
        });
    }
    Ok(RemoteCommitChangeDetails::Diff {
        diff: String::from_utf8_lossy(&diff).to_string(),
    })
}

fn append_diff_command(cwd: &Path, args: &[&str], diff: &mut Vec<u8>) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        return Err(command_error(
            "collect git diff for repo-ci remote commit decision",
            &format!("git {}", args.join(" ")),
            &output,
        ));
    }
    diff.extend_from_slice(&output.stdout);
    Ok(())
}

fn append_untracked_diff(cwd: &Path, path: &str, diff: &mut Vec<u8>) -> Result<()> {
    if !cwd.join(path).is_file() {
        return Ok(());
    }
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = Command::new("git")
        .args(["diff", "--no-index", "--", null_device, path])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to collect diff for untracked file {path}"))?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(command_error(
            "collect untracked git diff for repo-ci remote commit decision",
            &format!("git diff --no-index -- {null_device} {path}"),
            &output,
        ));
    }
    diff.extend_from_slice(&output.stdout);
    Ok(())
}

fn untracked_paths(cwd: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .current_dir(cwd)
        .output()
        .context("failed to list untracked files")?;
    if !output.status.success() {
        return Err(command_error(
            "list untracked files",
            "git ls-files --others --exclude-standard -z",
            &output,
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).to_string())
        .collect())
}

fn run_git_output(cwd: &Path, args: &[&str], action: &str) -> Result<Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(command_error(
            action,
            &format!("git {}", args.join(" ")),
            &output,
        ))
    }
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn command_error(action: &str, command_display: &str, output: &Output) -> anyhow::Error {
    anyhow!(
        "{action}: `{command_display}` exited with {}.\nstdout:\n{}\nstderr:\n{}",
        output.status,
        truncate_command_output(&output.stdout),
        truncate_command_output(&output.stderr),
    )
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

    #[test]
    fn commit_decision_context_includes_small_diff() {
        let (_temp, repo_root) = init_repo();
        fs::write(repo_root.join("base.txt"), "base\nchanged\n").expect("write change");

        let context = remote_commit_decision_context(&repo_root)
            .expect("context")
            .expect("dirty context");

        assert_eq!(context.changed_paths, vec!["base.txt".to_string()]);
        match context.change_details {
            RemoteCommitChangeDetails::Diff { diff } => {
                assert!(diff.contains("+changed"));
            }
            RemoteCommitChangeDetails::PathsOnly { .. } => {
                panic!("small diff should be included")
            }
        }
    }

    #[test]
    fn commit_decision_context_uses_paths_for_large_diff() {
        let (_temp, repo_root) = init_repo();
        fs::write(
            repo_root.join("base.txt"),
            "x".repeat(REMOTE_COMMIT_DECISION_MAX_DIFF_BYTES),
        )
        .expect("write large change");

        let context = remote_commit_decision_context(&repo_root)
            .expect("context")
            .expect("dirty context");

        assert_eq!(context.changed_paths, vec!["base.txt".to_string()]);
        match context.change_details {
            RemoteCommitChangeDetails::Diff { .. } => panic!("large diff should be omitted"),
            RemoteCommitChangeDetails::PathsOnly {
                diff_bytes,
                max_bytes,
            } => {
                assert!(diff_bytes > max_bytes);
                assert_eq!(max_bytes, REMOTE_COMMIT_DECISION_MAX_DIFF_BYTES);
            }
        }
    }

    #[test]
    fn apply_separate_commit_decision_commits_changes() {
        let (_temp, repo_root) = init_repo();
        fs::write(repo_root.join("base.txt"), "base\nchanged\n").expect("write change");
        let decision = RemoteCommitDecision {
            strategy: RemoteCommitStrategy::SeparateCommit,
            title: "repo-ci test commit".to_string(),
            body: "body".to_string(),
            rationale: "test".to_string(),
        };

        let applied = apply_remote_commit_decision(&repo_root, &decision)
            .expect("apply")
            .expect("applied");

        assert_eq!(
            applied,
            RemoteCommitApplied {
                strategy: RemoteCommitStrategy::SeparateCommit,
                title: Some("repo-ci test commit".to_string()),
            }
        );
        assert!(
            git_status_short(&repo_root)
                .expect("status")
                .trim()
                .is_empty()
        );
        let log = recent_commits(&repo_root).expect("recent commits");
        assert!(log.contains("repo-ci test commit"));
    }
}
