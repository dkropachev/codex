//! Executor-backed Git checks used to resolve and validate review targets.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathUri;

use crate::CommitLogEntry;
use crate::ReviewCommandOutput;
use crate::ReviewCommandRunner;
use crate::pull_request::run_git;

pub(crate) const REVIEW_SCOPE_COMMIT_LIMIT: usize = 100;
const DISABLED_HOOKS_PATH: &str = if cfg!(windows) { "NUL" } else { "/dev/null" };

fn safe_worktree_args<'a>(args: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    [
        "-c".to_string(),
        format!("core.hooksPath={DISABLED_HOOKS_PATH}"),
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
    ]
    .into_iter()
    .chain(args.into_iter().map(str::to_string))
    .collect()
}

/// Resolves the repository root containing `cwd` on the selected executor.
pub async fn resolve_review_repository_root(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Result<PathUri> {
    let output = run_git(runner, cwd, ["rev-parse", "--show-toplevel"])
        .await
        .context("failed to detect the review repository")?;
    require_success("git rev-parse --show-toplevel", &output)?;
    let root = output.stdout.trim();
    if root.is_empty() {
        bail!("`git rev-parse --show-toplevel` returned an empty path");
    }
    cwd.join(root)
        .with_context(|| format!("invalid review repository root {root:?}"))
}

/// Resolves the worktree Git directory and shared common directory on the selected executor.
pub async fn resolve_review_git_directories(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<Vec<PathUri>> {
    let output = run_git(
        runner,
        repository_root,
        [
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )
    .await
    .context("failed to resolve review Git directories")?;
    require_success("git rev-parse --git-dir --git-common-dir", &output)?;
    let mut directories = output
        .stdout
        .lines()
        .filter(|path| !path.trim().is_empty())
        .map(|path| repository_root.join(path.trim()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if directories.len() != 2 {
        bail!("`git rev-parse --git-dir --git-common-dir` returned invalid output");
    }
    directories.dedup();
    Ok(directories)
}

/// Validates that a review fix can create a new commit on the current branch.
pub async fn validate_review_fix_target(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<()> {
    resolve_commit_oid(runner, repository_root, "HEAD")
        .await
        .context("Fix requires an existing HEAD commit")?;
    Ok(())
}

/// Validates that a review fix can create a new commit on the current branch.
pub async fn validate_review_fix_commit_target(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<()> {
    let output = run_git(runner, repository_root, ["symbolic-ref", "--quiet", "HEAD"])
        .await
        .context("failed to inspect the review Fix + commit target")?;
    if output.exit_code != 0 || !output.stdout.trim().starts_with("refs/heads/") {
        bail!("Fix + commit requires a Git repository on an attached branch");
    }
    validate_review_fix_target(runner, repository_root)
        .await
        .context("Fix + commit requires an existing HEAD commit")?;
    Ok(())
}

/// Returns whether the selected repository has staged, unstaged, or untracked changes.
pub async fn has_uncommitted_changes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<bool> {
    let output = run_git_dynamic(
        runner,
        repository_root,
        safe_worktree_args(["status", "--porcelain=v1", "--untracked-files=all"]),
    )
    .await
    .context("failed to inspect uncommitted review changes")?;
    require_success("git status", &output)?;
    Ok(!output.stdout.trim().is_empty())
}

/// Returns whether the working tree differs from `base_sha`, including untracked files.
pub async fn has_changes_against_base(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    base_sha: &str,
) -> Result<bool> {
    let base_oid = resolve_commit_oid(runner, repository_root, base_sha).await?;
    let tracked = run_git_dynamic(
        runner,
        repository_root,
        safe_worktree_args(["diff", "--quiet", &base_oid, "--"]),
    )
    .await
    .context("failed to inspect tracked review changes")?;
    if diff_has_changes("git diff", &tracked)? {
        return Ok(true);
    }
    has_untracked_files(runner, repository_root).await
}

/// Returns whether `commit_sha` changes the tree relative to its first parent.
///
/// Root commits are compared with the empty tree. Merge commits are compared with their first
/// parent, matching the change that applying the commit to its mainline introduces.
pub async fn commit_has_changes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    commit_sha: &str,
) -> Result<bool> {
    let commit_oid = resolve_commit_oid(runner, repository_root, commit_sha).await?;
    resolved_commit_has_changes(runner, repository_root, &commit_oid).await
}

/// Returns whether an already-resolved commit object changes its first-parent tree.
pub async fn resolved_commit_has_changes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    commit_oid: &str,
) -> Result<bool> {
    let parents = run_git(
        runner,
        repository_root,
        ["rev-list", "--parents", "-n", "1", commit_oid],
    )
    .await
    .context("failed to inspect the review commit parents")?;
    require_success("git rev-list", &parents)?;
    let mut revisions = parents.stdout.split_whitespace();
    let Some(resolved_commit) = revisions.next() else {
        bail!("`git rev-list` returned no commit");
    };
    if resolved_commit != commit_oid {
        bail!("`git rev-list` returned an unexpected commit");
    }

    let (command, output) = if let Some(first_parent) = revisions.next() {
        (
            "git diff",
            run_git(
                runner,
                repository_root,
                ["diff", "--quiet", first_parent, commit_oid, "--"],
            )
            .await
            .context("failed to inspect review commit changes")?,
        )
    } else {
        (
            "git diff-tree",
            run_git(
                runner,
                repository_root,
                ["diff-tree", "--quiet", "--root", commit_oid, "--"],
            )
            .await
            .context("failed to inspect root review commit changes")?,
        )
    };
    diff_has_changes(command, &output)
}

/// Resolves an arbitrary review revision to its exact commit object ID.
pub async fn resolve_review_commit_oid(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    revision: &str,
) -> Result<String> {
    resolve_commit_oid(runner, repository_root, revision).await
}

pub(crate) async fn recent_review_commits(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<Vec<CommitLogEntry>> {
    let format = "--pretty=format:%H%x1f%ct%x1f%s";
    let output = run_git(runner, repository_root, ["log", "-n", "100", format])
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

async fn resolve_commit_oid(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    revision: &str,
) -> Result<String> {
    let revision = revision.trim();
    if revision.is_empty() {
        bail!("review revision must not be empty");
    }
    let commit_revision = format!("{revision}^{{commit}}");
    let output = run_git(
        runner,
        repository_root,
        [
            "rev-parse",
            "--verify",
            "--end-of-options",
            &commit_revision,
        ],
    )
    .await
    .with_context(|| format!("failed to resolve review revision {revision:?}"))?;
    require_success("git rev-parse --verify", &output)?;
    let oid = output.stdout.trim();
    if oid.is_empty() {
        bail!("`git rev-parse --verify` returned no commit");
    }
    Ok(oid.to_string())
}

async fn has_untracked_files(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
) -> Result<bool> {
    let output = run_git_dynamic(
        runner,
        repository_root,
        safe_worktree_args(["ls-files", "--others", "--exclude-standard"]),
    )
    .await
    .context("failed to inspect untracked review files")?;
    require_success("git ls-files", &output)?;
    Ok(!output.stdout.trim().is_empty())
}

async fn run_git_dynamic(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
    args: Vec<String>,
) -> Option<ReviewCommandOutput> {
    runner
        .run(
            crate::ReviewCommand::new(std::iter::once("git".to_string()).chain(args), cwd.clone())
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C"),
        )
        .await
        .ok()
}

fn diff_has_changes(command: &str, output: &ReviewCommandOutput) -> Result<bool> {
    match output.exit_code {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(command_error(command, output)),
    }
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
