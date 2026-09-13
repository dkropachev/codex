use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tokio::sync::Barrier;

use super::*;

#[tokio::test]
async fn current_branch_pull_request_wins_and_its_base_is_preferred() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 0,
            r#"{"number":42,"url":"https://github.com/acme/repo/pull/42","state":"OPEN","baseRefName":"develop"}"#,
        ),
        response(["git", "remote"], /*exit_code*/ 0, "origin\n"),
        response(
            ["git", "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            /*exit_code*/ 0,
            "refs/remotes/origin/main\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main",
            ],
            /*exit_code*/ 0,
            "main-sha\n",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/main\nrefs/heads/develop\nrefs/heads/feature\n",
        ),
        response(["git", "remote"], /*exit_code*/ 0, "origin\n"),
        response(
            ["git", "remote", "get-url", "origin"],
            /*exit_code*/ 0,
            "https://github.com/acme/repo.git\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "refs/remotes/origin/develop^{commit}",
            ],
            /*exit_code*/ 0,
            "develop-sha\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution,
        ReviewScopeResolution {
            pull_request: Some(ReviewScopePullRequest {
                number: 42,
                url: "https://github.com/acme/repo/pull/42".to_string(),
                base_branch: Some("develop".to_string()),
                base_branch_target: Some("refs/remotes/origin/develop".to_string()),
            }),
            default_branch: Some(ReviewDefaultBranch {
                display_name: "main".to_string(),
                target: "refs/remotes/origin/main".to_string(),
            }),
            current_branch: Some("feature".to_string()),
            branches: vec![
                "refs/remotes/origin/develop".to_string(),
                "refs/heads/feature".to_string(),
                "refs/heads/main".to_string(),
            ],
            has_uncommitted_changes: false,
            commits: vec![CommitLogEntry {
                sha: "commit-sha".to_string(),
                timestamp: 1,
                subject: "Commit subject".to_string(),
            }],
            git_error: None,
        }
    );
    assert!(!runner.saw(&["git", "rev-parse", "HEAD"]));
    runner.assert_exhausted();
}

#[tokio::test]
async fn pull_request_base_matching_default_uses_base_repository_remote() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 0,
            r#"{"number":42,"url":"https://github.com/acme/repo/pull/42","state":"OPEN","baseRefName":"main"}"#,
        ),
        response(
            ["git", "remote"],
            /*exit_code*/ 0,
            "origin\nupstream\n",
        ),
        response(
            ["git", "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            /*exit_code*/ 0,
            "refs/remotes/origin/main\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main",
            ],
            /*exit_code*/ 0,
            "main-sha\n",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/main\nrefs/heads/feature\n",
        ),
        response(
            ["git", "remote"],
            /*exit_code*/ 0,
            "origin\nupstream\n",
        ),
        response(
            ["git", "remote", "get-url", "origin"],
            /*exit_code*/ 0,
            "https://github.com/example/repo-fork.git\n",
        ),
        response(
            ["git", "remote", "get-url", "upstream"],
            /*exit_code*/ 0,
            "https://github.com/acme/repo.git\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "refs/remotes/upstream/main^{commit}",
            ],
            /*exit_code*/ 0,
            "main-sha\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution.pull_request,
        Some(ReviewScopePullRequest {
            number: 42,
            url: "https://github.com/acme/repo/pull/42".to_string(),
            base_branch: Some("main".to_string()),
            base_branch_target: Some("refs/remotes/upstream/main".to_string()),
        })
    );
    assert_eq!(
        resolution.branches,
        vec!["refs/remotes/upstream/main", "refs/heads/feature"]
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn head_lookup_searches_parent_before_fork_and_uses_lowest_open_number() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 1,
            "",
        ),
        response(
            ["git", "rev-parse", "HEAD"],
            /*exit_code*/ 0,
            "head-sha\n",
        ),
        response(
            ["gh", "repo", "view", "--json", "nameWithOwner,parent"],
            /*exit_code*/ 0,
            r#"{"nameWithOwner":"fork/repo","parent":{"nameWithOwner":"upstream/repo"}}"#,
        ),
        response(
            [
                "gh",
                "api",
                "--paginate",
                "--slurp",
                "-H",
                "Accept: application/vnd.github+json",
                "repos/upstream/repo/commits/head-sha/pulls",
            ],
            /*exit_code*/ 0,
            r#"[[{"number":1,"html_url":"https://github.com/upstream/repo/pull/1","state":"closed","base":{"ref":"main"}}]]"#,
        ),
        response(
            [
                "gh",
                "api",
                "--paginate",
                "--slurp",
                "-H",
                "Accept: application/vnd.github+json",
                "repos/fork/repo/commits/head-sha/pulls",
            ],
            /*exit_code*/ 0,
            r#"[[{"number":9,"html_url":"https://github.com/fork/repo/pull/9","state":"open","base":{"ref":"main"}}],[{"number":2,"html_url":"https://github.com/fork/repo/pull/2","state":"OPEN","base":{"ref":"trunk"}}]]"#,
        ),
        response(["git", "remote"], /*exit_code*/ 0, ""),
        response(
            ["git", "rev-parse", "--verify", "--quiet", "refs/heads/main"],
            /*exit_code*/ 1,
            "",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/master",
            ],
            /*exit_code*/ 1,
            "",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/feature\n",
        ),
        response(["git", "remote"], /*exit_code*/ 0, "origin\n"),
        response(
            ["git", "remote", "get-url", "origin"],
            /*exit_code*/ 0,
            "https://github.com/fork/repo.git\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--end-of-options",
                "refs/remotes/origin/trunk^{commit}",
            ],
            /*exit_code*/ 0,
            "trunk-sha\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution.pull_request,
        Some(ReviewScopePullRequest {
            number: 2,
            url: "https://github.com/fork/repo/pull/2".to_string(),
            base_branch: Some("trunk".to_string()),
            base_branch_target: Some("refs/remotes/origin/trunk".to_string()),
        })
    );
    assert_eq!(
        resolution.branches,
        vec!["refs/remotes/origin/trunk", "refs/heads/feature"]
    );
    let seen = runner.seen();
    let parent = seen
        .iter()
        .position(|argv| {
            argv.last()
                .is_some_and(|arg| arg.starts_with("repos/upstream/"))
        })
        .expect("parent lookup");
    let fork = seen
        .iter()
        .position(|argv| {
            argv.last()
                .is_some_and(|arg| arg.starts_with("repos/fork/"))
        })
        .expect("fork lookup");
    assert!(parent < fork);
    runner.assert_exhausted();
}

#[tokio::test]
async fn detected_default_branch_is_inserted_once_at_the_front() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 1,
            "",
        ),
        response(["git", "rev-parse", "HEAD"], /*exit_code*/ 1, ""),
        response(["git", "remote"], /*exit_code*/ 0, "origin\n"),
        response(
            ["git", "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            /*exit_code*/ 0,
            "refs/remotes/origin/main\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main",
            ],
            /*exit_code*/ 0,
            "main-sha\n",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/topic\nrefs/heads/main\nrefs/heads/feature\nrefs/heads/main\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution.default_branch,
        Some(ReviewDefaultBranch {
            display_name: "main".to_string(),
            target: "refs/remotes/origin/main".to_string(),
        })
    );
    assert_eq!(
        resolution.branches,
        vec![
            "refs/remotes/origin/main",
            "refs/heads/feature",
            "refs/heads/topic",
        ]
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn remote_show_default_branch_uses_verified_remote_target() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 1,
            "",
        ),
        response(["git", "rev-parse", "HEAD"], /*exit_code*/ 1, ""),
        response(["git", "remote"], /*exit_code*/ 0, "origin\n"),
        response(
            ["git", "symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            /*exit_code*/ 1,
            "",
        ),
        response(
            ["git", "remote", "show", "origin"],
            /*exit_code*/ 0,
            "  HEAD branch: trunk\n",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/trunk",
            ],
            /*exit_code*/ 0,
            "trunk-sha\n",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/feature\nrefs/heads/trunk\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution.default_branch,
        Some(ReviewDefaultBranch {
            display_name: "trunk".to_string(),
            target: "refs/remotes/origin/trunk".to_string(),
        })
    );
    assert_eq!(
        resolution.branches,
        vec!["refs/remotes/origin/trunk", "refs/heads/feature"]
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn local_default_branch_is_used_when_remote_detection_fails() {
    let runner = scope_runner(vec![
        response(
            ["gh", "pr", "view", "--json", "number,url,state,baseRefName"],
            /*exit_code*/ 1,
            "",
        ),
        response(["git", "rev-parse", "HEAD"], /*exit_code*/ 1, ""),
        response(["git", "remote"], /*exit_code*/ 0, ""),
        response(
            ["git", "rev-parse", "--verify", "--quiet", "refs/heads/main"],
            /*exit_code*/ 1,
            "",
        ),
        response(
            [
                "git",
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/master",
            ],
            /*exit_code*/ 0,
            "master-sha\n",
        ),
        response(
            ["git", "branch", "--show-current"],
            /*exit_code*/ 0,
            "feature\n",
        ),
        response(
            ["git", "for-each-ref", "--format=%(refname)", "refs/heads"],
            /*exit_code*/ 0,
            "refs/heads/feature\nrefs/heads/master\n",
        ),
    ]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution.default_branch,
        Some(ReviewDefaultBranch {
            display_name: "master".to_string(),
            target: "refs/heads/master".to_string(),
        })
    );
    assert_eq!(
        resolution.branches,
        vec!["refs/heads/master", "refs/heads/feature"]
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn git_detection_failure_returns_a_short_picker_error() {
    let runner = FakeRunner::new(vec![response(
        ["git", "rev-parse", "--show-toplevel"],
        /*exit_code*/ 128,
        "",
    )]);

    let resolution = resolve_review_scope(&runner, &cwd()).await;

    assert_eq!(
        resolution,
        ReviewScopeResolution {
            git_error: Some("Git detection failed".to_string()),
            ..Default::default()
        }
    );
    runner.assert_exhausted();
}

#[tokio::test]
async fn pull_request_and_default_branch_probes_start_concurrently() {
    let runner = ConcurrentProbeRunner {
        barrier: Arc::new(Barrier::new(2)),
    };

    let resolution = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 1),
        resolve_review_scope(&runner, &cwd()),
    )
    .await
    .expect("PR and default-branch probes should not wait for one another");

    assert_eq!(resolution.pull_request.expect("PR").number, 7);
    assert_eq!(
        resolution.default_branch,
        Some(ReviewDefaultBranch {
            display_name: "main".to_string(),
            target: "refs/heads/main".to_string(),
        })
    );
}

fn cwd() -> PathUri {
    PathUri::parse("file:///repo").expect("cwd")
}

fn scope_runner(mut responses: Vec<FakeResponse>) -> FakeRunner {
    responses.extend([
        response(
            ["git", "rev-parse", "--show-toplevel"],
            /*exit_code*/ 0,
            "/repo\n",
        ),
        response(safe_status_argv(), /*exit_code*/ 0, ""),
        response(
            ["git", "log", "-n", "100", "--pretty=format:%H%x1f%ct%x1f%s"],
            /*exit_code*/ 0,
            "commit-sha\u{001f}1\u{001f}Commit subject\n",
        ),
    ]);
    FakeRunner::new(responses)
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

fn safe_status_argv() -> Vec<String> {
    let hooks_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
    [
        "git".to_string(),
        "-c".to_string(),
        format!("core.hooksPath={hooks_path}"),
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
        "status".to_string(),
        "--porcelain=v1".to_string(),
        "--untracked-files=all".to_string(),
    ]
    .into()
}

struct FakeResponse {
    argv: Vec<String>,
    output: ReviewCommandOutput,
}

struct FakeRunner {
    responses: Mutex<VecDeque<FakeResponse>>,
    seen: Mutex<Vec<Vec<String>>>,
}

impl FakeRunner {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn saw(&self, argv: &[&str]) -> bool {
        let argv = argv
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        self.seen().contains(&argv)
    }

    fn seen(&self) -> Vec<Vec<String>> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn assert_exhausted(&self) {
        assert_eq!(self.responses.lock().expect("responses lock").len(), 0);
    }
}

impl ReviewCommandRunner for FakeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        self.seen
            .lock()
            .expect("seen lock")
            .push(command.argv().to_vec());
        let mut responses = self.responses.lock().expect("responses lock");
        let index = responses
            .iter()
            .position(|response| response.argv == command.argv())
            .unwrap_or_else(|| panic!("missing fake response for {:?}", command.argv()));
        Ok(responses.remove(index).expect("fake response").output)
    }
}

struct ConcurrentProbeRunner {
    barrier: Arc<Barrier>,
}

impl ReviewCommandRunner for ConcurrentProbeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        if command.argv() == ["gh", "pr", "view", "--json", "number,url,state,baseRefName"]
            || command.argv() == ["git", "remote"]
        {
            self.barrier.wait().await;
        }
        let (exit_code, stdout) = match command.argv() {
            [program, command, option]
                if program == "git" && command == "rev-parse" && option == "--show-toplevel" =>
            {
                (0, "/repo\n")
            }
            [program, command, ..] if program == "gh" && command == "pr" => (
                0,
                r#"{"number":7,"url":"https://github.com/acme/repo/pull/7","state":"OPEN"}"#,
            ),
            [program, command] if program == "git" && command == "remote" => (0, ""),
            [program, command, _, _, reference]
                if program == "git" && command == "rev-parse" && reference == "refs/heads/main" =>
            {
                (0, "main-sha\n")
            }
            [program, command, _, reference]
                if program == "git"
                    && command == "rev-parse"
                    && reference == "refs/heads/main^{commit}" =>
            {
                (0, "main-sha\n")
            }
            [program, command, ..] if program == "git" && command == "for-each-ref" => {
                (0, "refs/heads/main\n")
            }
            [program, command, ..] if program == "git" && command == "branch" => (0, "feature\n"),
            args if args.first().is_some_and(|program| program == "git")
                && args.iter().any(|arg| arg == "status") =>
            {
                (0, "")
            }
            [program, command, ..] if program == "git" && command == "log" => (0, ""),
            _ => (1, ""),
        };
        Ok(ReviewCommandOutput {
            exit_code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }
}
