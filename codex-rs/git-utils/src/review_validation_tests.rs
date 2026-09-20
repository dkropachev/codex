use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::Result;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;

use super::*;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;

#[tokio::test]
async fn resolves_repository_root_using_executor_path_convention() {
    let runner = FakeRunner::new(vec![response(
        ["git", "rev-parse", "--show-toplevel"],
        /*exit_code*/ 0,
        "C:\\workspace\n",
    )]);
    let cwd = PathUri::parse("file:///C:/workspace/subdir").expect("cwd");

    assert_eq!(
        resolve_review_repository_root(&runner, &cwd)
            .await
            .expect("repository root"),
        PathUri::parse("file:///C:/workspace").expect("repository root URI")
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn resolves_executor_git_and_common_directories() {
    let runner = FakeRunner::new(vec![response(
        [
            "git",
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
        /*exit_code*/ 0,
        "C:\\repo.git\\worktrees\\feature\nC:\\repo.git\n",
    )]);
    let repository = PathUri::parse("file:///C:/workspace").expect("repository URI");

    assert_eq!(
        resolve_review_git_directories(&runner, &repository)
            .await
            .expect("Git directories"),
        vec![
            PathUri::parse("file:///C:/repo.git/worktrees/feature").expect("git dir"),
            PathUri::parse("file:///C:/repo.git").expect("common dir"),
        ]
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn fix_commit_target_requires_an_attached_branch() {
    let attached = FakeRunner::new(vec![
        response(
            ["git", "symbolic-ref", "--quiet", "HEAD"],
            /*exit_code*/ 0,
            "refs/heads/feature\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "HEAD^{commit}",
            ],
            /*exit_code*/ 0,
            "abc123\n",
        ),
    ]);
    validate_review_fix_commit_target(&attached, &cwd())
        .await
        .expect("attached branch");
    attached.assert_exhausted();

    let detached = FakeRunner::new(vec![response(
        ["git", "symbolic-ref", "--quiet", "HEAD"],
        /*exit_code*/ 1,
        "",
    )]);
    let error = validate_review_fix_commit_target(&detached, &cwd())
        .await
        .expect_err("detached HEAD");
    assert!(error.to_string().contains("attached branch"));
    detached.assert_exhausted();

    let tag = FakeRunner::new(vec![response(
        ["git", "symbolic-ref", "--quiet", "HEAD"],
        /*exit_code*/ 0,
        "refs/tags/v1\n",
    )]);
    let error = validate_review_fix_commit_target(&tag, &cwd())
        .await
        .expect_err("symbolic tag HEAD");
    assert!(error.to_string().contains("attached branch"));
    tag.assert_exhausted();
}

#[tokio::test]
async fn fix_target_requires_an_existing_head_commit() {
    let unborn = FakeRunner::new(vec![response(
        [
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            "HEAD^{commit}",
        ],
        /*exit_code*/ 128,
        "",
    )]);

    let error = validate_review_fix_target(&unborn, &cwd())
        .await
        .expect_err("unborn HEAD");

    assert!(format!("{error:#}").contains("existing HEAD commit"));
    unborn.assert_exhausted();
}

#[tokio::test]
async fn detects_uncommitted_changes_from_porcelain_status() {
    let runner = FakeRunner::new(vec![response(
        safe_args(&["status", "--porcelain=v1", "--untracked-files=all"]),
        /*exit_code*/ 0,
        "?? new.rs\n",
    )]);

    assert!(
        has_uncommitted_changes(&runner, &cwd())
            .await
            .expect("uncommitted changes")
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn detects_untracked_changes_when_tracked_tree_matches_base() {
    let runner = FakeRunner::new(vec![
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "base^{commit}",
            ],
            /*exit_code*/ 0,
            "base-oid\n",
        ),
        response(
            safe_args(&["diff", "--quiet", "base-oid", "--"]),
            /*exit_code*/ 0,
            "",
        ),
        response(
            safe_args(&["ls-files", "--others", "--exclude-standard"]),
            /*exit_code*/ 0,
            "new.rs\n",
        ),
    ]);

    assert!(
        has_changes_against_base(&runner, &cwd(), "base")
            .await
            .expect("changes against base")
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn tracked_changes_short_circuit_untracked_scan() {
    let runner = FakeRunner::new(vec![
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "base^{commit}",
            ],
            /*exit_code*/ 0,
            "base-oid\n",
        ),
        response(
            safe_args(&["diff", "--quiet", "base-oid", "--"]),
            /*exit_code*/ 1,
            "",
        ),
    ]);

    assert!(
        has_changes_against_base(&runner, &cwd(), "base")
            .await
            .expect("changes against base")
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn validates_regular_and_root_commit_changes() {
    let regular = FakeRunner::new(vec![
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "commit^{commit}",
            ],
            /*exit_code*/ 0,
            "commit-oid\n",
        ),
        response(
            ["git", "rev-list", "--parents", "-n", "1", "commit-oid"],
            /*exit_code*/ 0,
            "commit-oid parent-oid\n",
        ),
        response(
            ["git", "diff", "--quiet", "parent-oid", "commit-oid", "--"],
            /*exit_code*/ 1,
            "",
        ),
    ]);
    assert!(
        commit_has_changes(&regular, &cwd(), "commit")
            .await
            .expect("regular commit changes")
    );
    regular.assert_exhausted();

    let empty_root = FakeRunner::new(vec![
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "root^{commit}",
            ],
            /*exit_code*/ 0,
            "root-oid\n",
        ),
        response(
            ["git", "rev-list", "--parents", "-n", "1", "root-oid"],
            /*exit_code*/ 0,
            "root-oid\n",
        ),
        response(
            ["git", "diff-tree", "--quiet", "--root", "root-oid", "--"],
            /*exit_code*/ 0,
            "",
        ),
    ]);
    assert!(
        !commit_has_changes(&empty_root, &cwd(), "root")
            .await
            .expect("empty root commit")
    );
    empty_root.assert_exhausted();
}

#[tokio::test]
async fn validation_errors_on_inconclusive_git_exit_status() {
    let runner = FakeRunner::new(vec![
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "base^{commit}",
            ],
            /*exit_code*/ 0,
            "base-oid\n",
        ),
        failed_response(
            safe_args(&["diff", "--quiet", "base-oid", "--"]),
            /*exit_code*/ 2,
            "bad revision",
        ),
    ]);

    let error = has_changes_against_base(&runner, &cwd(), "base")
        .await
        .expect_err("inconclusive diff must fail validation");

    assert!(format!("{error:#}").contains("bad revision"));
    runner.assert_exhausted();
}

#[tokio::test]
async fn recent_review_commits_are_hard_capped() {
    let stdout = (0..=REVIEW_SCOPE_COMMIT_LIMIT)
        .map(|index| format!("sha-{index}\u{001f}{index}\u{001f}subject {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let runner = FakeRunner::new(vec![response(
        ["git", "log", "-n", "100", "--pretty=format:%H%x1f%ct%x1f%s"],
        /*exit_code*/ 0,
        &stdout,
    )]);

    let commits = recent_review_commits(&runner, &cwd())
        .await
        .expect("recent commits");

    assert_eq!(commits.len(), REVIEW_SCOPE_COMMIT_LIMIT);
    assert_eq!(commits[0].sha, "sha-0");
    assert_eq!(commits[REVIEW_SCOPE_COMMIT_LIMIT - 1].sha, "sha-99");
    runner.assert_exhausted();
}

#[cfg(unix)]
#[tokio::test]
async fn worktree_scan_disables_repository_fsmonitor_helper() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::TempDir::new().expect("temp repo");
    run_native(root.path(), &["init", "--initial-branch=main"]);
    let marker = root.path().join("fsmonitor-ran");
    let helper = root.path().join("fsmonitor.sh");
    std::fs::write(
        &helper,
        format!("#!/bin/sh\ntouch '{}'\nprintf '0\\n'\n", marker.display()),
    )
    .expect("write helper");
    let mut permissions = std::fs::metadata(&helper)
        .expect("helper metadata")
        .permissions();
    permissions.set_mode(/*mode*/ 0o755);
    std::fs::set_permissions(&helper, permissions).expect("make helper executable");
    run_native(
        root.path(),
        &[
            "config",
            "core.fsmonitor",
            helper.to_str().expect("helper path"),
        ],
    );
    let cwd = PathUri::from_host_native_path(root.path()).expect("repo URI");

    let dirty = has_uncommitted_changes(&NativeRunner, &cwd)
        .await
        .expect("scan worktree");

    assert!(dirty);
    assert!(!marker.exists(), "repository fsmonitor helper executed");
}

fn cwd() -> PathUri {
    PathUri::parse("file:///repo").expect("cwd")
}

fn safe_args(args: &[&str]) -> Vec<String> {
    let disabled_hooks = if cfg!(windows) { "NUL" } else { "/dev/null" };
    std::iter::once("git")
        .map(str::to_string)
        .chain([
            "-c".to_string(),
            format!("core.hooksPath={disabled_hooks}"),
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
        ])
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect()
}

#[cfg(unix)]
struct NativeRunner;

#[cfg(unix)]
impl crate::ReviewCommandRunner for NativeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let cwd = command.cwd().to_abs_path()?;
        let output = tokio::process::Command::new(&command.argv()[0])
            .args(&command.argv()[1..])
            .current_dir(cwd.as_path())
            .envs(command.env_vars())
            .output()
            .await?;
        Ok(ReviewCommandOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(unix)]
fn run_native(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn response(
    argv: impl IntoIterator<Item = impl ToString>,
    exit_code: i32,
    stdout: &str,
) -> FakeResponse {
    FakeResponse {
        argv: argv.into_iter().map(|arg| arg.to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        },
    }
}

fn failed_response(
    argv: impl IntoIterator<Item = impl ToString>,
    exit_code: i32,
    stderr: &str,
) -> FakeResponse {
    FakeResponse {
        argv: argv.into_iter().map(|arg| arg.to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code,
            stdout: String::new(),
            stderr: stderr.to_string(),
        },
    }
}

struct FakeResponse {
    argv: Vec<String>,
    output: ReviewCommandOutput,
}

struct FakeRunner {
    responses: Mutex<VecDeque<FakeResponse>>,
}

impl FakeRunner {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }

    fn assert_exhausted(&self) {
        assert_eq!(self.responses.lock().expect("responses lock").len(), 0);
    }
}

impl ReviewCommandRunner for FakeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("unexpected review command");
        assert_eq!(command.argv(), response.argv);
        Ok(response.output)
    }
}
