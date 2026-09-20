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

/// Resolves the repository root containing `cwd` on the selected executor.
pub async fn resolve_review_repository_root(
    runner: &impl ReviewCommandRunner,
    cwd: &PathUri,
) -> Result<PathUri> {
    let inside_worktree = run_git(runner, cwd, ["rev-parse", "--is-inside-work-tree"])
        .await
        .context("failed to detect the review worktree")?;
    require_success("git rev-parse --is-inside-work-tree", &inside_worktree)?;
    if inside_worktree.stdout.trim() != "true" {
        bail!("review cwd is not inside a Git worktree");
    }
    let output = run_git(runner, cwd, ["rev-parse", "--show-cdup"])
        .await
        .context("failed to detect the review repository")?;
    require_success("git rev-parse --show-cdup", &output)?;
    let relative_root = output
        .stdout
        .strip_suffix('\n')
        .context("`git rev-parse --show-cdup` returned unterminated output")?;
    let relative_root = if cwd.infer_path_convention() == Some(PathConvention::Windows) {
        relative_root.strip_suffix('\r').unwrap_or(relative_root)
    } else {
        relative_root
    };
    if !relative_root.is_empty()
        && (!relative_root.ends_with('/')
            || relative_root
                .split('/')
                .filter(|segment| !segment.is_empty())
                .any(|segment| segment != ".."))
    {
        bail!("`git rev-parse --show-cdup` returned an invalid path");
    }
    let parent_count = relative_root
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count();
    let mut root = cwd.clone();
    for _ in 0..parent_count {
        root = root
            .parent()
            .context("review repository root escaped the selected cwd")?;
    }
    Ok(root)
}

/// Resolves an arbitrary review revision to its exact commit object ID.
pub async fn resolve_review_commit_oid(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    revision: &str,
) -> Result<String> {
    let oid = crate::pull_request::resolve_revision_oid(runner, repository_root, revision)
        .await?
        .with_context(|| format!("review revision {revision:?} does not resolve to a commit"))?;
    if !is_object_id(&oid) {
        bail!("Git returned an invalid review commit object ID");
    }
    Ok(oid)
}

/// Returns whether the selected branch scope contains committed or worktree changes.
pub async fn has_changes_against_base(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    base_sha: &str,
) -> Result<bool> {
    let base_oid = resolve_review_commit_oid(runner, repository_root, base_sha).await?;
    let committed = run_git(
        runner,
        repository_root,
        [
            "diff-tree",
            "--quiet",
            "--ignore-submodules=none",
            "-r",
            &base_oid,
            "HEAD",
            "--",
        ],
    )
    .await
    .context("failed to inspect committed review changes")?;
    if diff_has_changes("git diff-tree", &committed)? {
        return Ok(true);
    }
    has_uncommitted_changes(runner, repository_root).await
}

/// Returns whether an already-resolved commit changes its first-parent tree.
pub async fn resolved_commit_has_changes(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    commit_oid: &str,
) -> Result<bool> {
    if !is_object_id(commit_oid) {
        bail!("invalid review commit object ID");
    }
    let commit = run_git(
        runner,
        repository_root,
        ["--no-replace-objects", "cat-file", "-p", commit_oid],
    )
    .await
    .context("failed to inspect the review commit")?;
    require_success("git cat-file", &commit)?;
    let mut headers = commit.stdout.lines().take_while(|line| !line.is_empty());
    let tree = headers
        .next()
        .context("`git cat-file` returned no commit headers")?;
    let Some(tree_oid) = tree.strip_prefix("tree ") else {
        bail!("`git cat-file` returned an invalid commit object");
    };
    if !is_object_id(tree_oid) {
        bail!("`git cat-file` returned an invalid tree object ID");
    }
    let first_parent = headers.next().and_then(|line| line.strip_prefix("parent "));
    if first_parent.is_some_and(|parent| !is_object_id(parent)) {
        bail!("`git cat-file` returned an invalid parent object ID");
    }

    if let Some(first_parent) = first_parent {
        let output = run_git(
            runner,
            repository_root,
            [
                "--no-replace-objects",
                "diff-tree",
                "--quiet",
                "--ignore-submodules=none",
                "-r",
                first_parent,
                commit_oid,
                "--",
            ],
        )
        .await
        .context("failed to inspect review commit changes")?;
        diff_has_changes("git diff-tree", &output)
    } else {
        let output = run_git(
            runner,
            repository_root,
            [
                "--no-replace-objects",
                "diff-tree",
                "--quiet",
                "--ignore-submodules=none",
                "--root",
                "-r",
                commit_oid,
                "--",
            ],
        )
        .await
        .context("failed to inspect root review commit changes")?;
        diff_has_changes("git diff-tree", &output)
    }
}

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
        .env("GIT_NO_REPLACE_OBJECTS", "1")
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

fn diff_has_changes(command: &str, output: &ReviewCommandOutput) -> Result<bool> {
    match output.exit_code {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(command_error(command, output)),
    }
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
