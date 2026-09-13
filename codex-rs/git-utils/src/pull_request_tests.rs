use std::collections::VecDeque;
use std::sync::Mutex;

use pretty_assertions::assert_eq;

use super::*;

const PULL_REQUEST_URL: &str = "https://github.com/openai/codex/pull/42";

#[test]
fn parses_pull_request_metadata() {
    let metadata =
        parse_pull_request_metadata(gh_output().as_bytes()).expect("pull request metadata");

    assert_eq!(metadata, expected_metadata());
}

#[test]
fn rejects_pull_request_that_is_no_longer_open() {
    let mut metadata = expected_metadata();
    metadata.state = "MERGED".to_string();

    let error = ensure_pull_request_is_open(&metadata).expect_err("merged pull request");

    assert_eq!(
        error.to_string(),
        "pull request https://github.com/openai/codex/pull/42 is no longer open (state: MERGED)"
    );
}

#[test]
fn rejects_pull_request_metadata_without_a_base() {
    let error = parse_pull_request_metadata(
        br#"{
            "number": 42,
            "title": "Missing base",
            "body": "",
            "url": "https://github.com/openai/codex/pull/42",
            "state": "OPEN",
            "baseRefName": "",
            "baseRefOid": "",
            "headRefOid": "head-oid"
        }"#,
    )
    .expect_err("missing pull request base");

    assert_eq!(error.to_string(), "GitHub returned no pull request base");
}

#[test]
fn pull_request_url_identity_preserves_server_port() {
    let first = canonical_pull_request_url("https://ghe.example:8443/org/repo/pull/42")
        .expect("first pull request URL");
    let second = canonical_pull_request_url("https://ghe.example:9443/org/repo/pull/42")
        .expect("second pull request URL");

    assert_ne!(first, second);
}

#[tokio::test]
async fn resolver_prefers_pull_request_base_oid() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 0,
            "resolved-base-oid\n",
            "",
        ),
        response(
            git_argv(&["merge-base", "HEAD", "resolved-base-oid"]),
            /*exit_code*/ 0,
            "merge-base-oid\n",
            "",
        ),
    ]);

    let resolved = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect("resolved pull request");

    assert_eq!(
        resolved,
        ResolvedPullRequestReview {
            metadata: expected_metadata(),
            merge_base: "merge-base-oid".to_string(),
        }
    );
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_falls_back_to_unique_remote_pull_request_base_ref() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 1,
            "",
            "missing oid",
        ),
        response(git_argv(&["remote"]), /*exit_code*/ 0, "origin\n", ""),
        response(
            git_argv(&["remote", "get-url", "origin"]),
            /*exit_code*/ 0,
            "https://github.com/openai/codex.git\n",
            "",
        ),
        response(
            git_argv(&["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"]),
            /*exit_code*/ 0,
            "resolved-main\n",
            "",
        ),
        response(
            git_argv(&["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"]),
            /*exit_code*/ 0,
            "resolved-main\n",
            "",
        ),
        response(
            git_argv(&["merge-base", "HEAD", "resolved-main"]),
            /*exit_code*/ 0,
            "merge-base-main\n",
            "",
        ),
    ]);

    let resolved = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect("resolved pull request");

    assert_eq!(resolved.merge_base, "merge-base-main");
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_does_not_fall_back_when_base_oid_has_no_merge_base() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 0,
            "resolved-base-oid\n",
            "",
        ),
        response(
            git_argv(&["merge-base", "HEAD", "resolved-base-oid"]),
            /*exit_code*/ 1,
            "",
            "no common ancestor",
        ),
    ]);

    let error = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect_err("unrelated pull request base");

    let error = format!("{error:#}");
    assert!(error.contains("failed to resolve base object ID \"base-oid\""));
    assert!(error.contains("no common ancestor"));
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_fails_when_oid_and_ref_cannot_be_resolved() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 1,
            "",
            "missing oid",
        ),
        response(git_argv(&["remote"]), /*exit_code*/ 0, "", ""),
    ]);

    let error = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect_err("unresolvable pull request base");

    let error = error.to_string();
    assert!(error.contains("failed to resolve the base for pull request"));
    assert!(error.contains("base-oid"));
    assert!(error.contains("main"));
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_rejects_divergent_base_repository_refs() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 1,
            "",
            "missing oid",
        ),
        response(
            git_argv(&["remote"]),
            /*exit_code*/ 0,
            "origin\nupstream\n",
            "",
        ),
        response(
            git_argv(&["remote", "get-url", "origin"]),
            /*exit_code*/ 0,
            "https://github.com/openai/codex.git\n",
            "",
        ),
        response(
            git_argv(&["remote", "get-url", "upstream"]),
            /*exit_code*/ 0,
            "git@github.com:openai/codex.git\n",
            "",
        ),
        response(
            git_argv(&["rev-parse", "--verify", "refs/remotes/origin/main^{commit}"]),
            /*exit_code*/ 0,
            "origin-main\n",
            "",
        ),
        response(
            git_argv(&[
                "rev-parse",
                "--verify",
                "refs/remotes/upstream/main^{commit}",
            ]),
            /*exit_code*/ 0,
            "upstream-main\n",
            "",
        ),
    ]);

    let error = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect_err("ambiguous pull request base");

    let error = format!("{error:#}");
    assert!(error.contains("branch \"main\" is ambiguous"));
    assert!(error.contains("refs/remotes/origin/main"));
    assert!(error.contains("refs/remotes/upstream/main"));
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_does_not_use_same_named_branch_from_fork_remote() {
    let runner = FakeRunner::new(vec![
        response(gh_argv(), /*exit_code*/ 0, &gh_output(), ""),
        response(
            git_argv(&["rev-parse", "--verify", "base-oid^{commit}"]),
            /*exit_code*/ 1,
            "",
            "missing oid",
        ),
        response(git_argv(&["remote"]), /*exit_code*/ 0, "origin\n", ""),
        response(
            git_argv(&["remote", "get-url", "origin"]),
            /*exit_code*/ 0,
            "https://github.com/example/codex-fork.git\n",
            "",
        ),
    ]);

    let error = resolve_pull_request_for_review_with_runner(&runner, &cwd(), PULL_REQUEST_URL)
        .await
        .expect_err("fork remote must not stand in for PR base repository");

    assert!(
        error
            .to_string()
            .contains("base ref \"main\" was not found")
    );
    runner.assert_finished();
}

#[tokio::test]
async fn resolver_rejects_option_shaped_and_mismatched_pull_request_urls() {
    let invalid_runner = FakeRunner::new(Vec::new());
    let invalid =
        resolve_pull_request_for_review_with_runner(&invalid_runner, &cwd(), "-Rother/repo")
            .await
            .expect_err("option-shaped selector");
    assert!(invalid.to_string().contains("absolute URL"));
    invalid_runner.assert_finished();

    let mismatched_output =
        gh_output().replace(PULL_REQUEST_URL, "https://github.com/openai/codex/pull/99");
    let mismatched_runner = FakeRunner::new(vec![response(
        gh_argv(),
        /*exit_code*/ 0,
        &mismatched_output,
        "",
    )]);
    let mismatched =
        resolve_pull_request_for_review_with_runner(&mismatched_runner, &cwd(), PULL_REQUEST_URL)
            .await
            .expect_err("mismatched resolved URL");
    assert!(
        mismatched
            .to_string()
            .contains("different pull request URL")
    );
    mismatched_runner.assert_finished();
}

fn cwd() -> PathUri {
    PathUri::parse("file:///workspace").expect("cwd URI")
}

fn gh_argv() -> Vec<String> {
    vec![
        "gh".to_string(),
        "pr".to_string(),
        "view".to_string(),
        PULL_REQUEST_URL.to_string(),
        "--json".to_string(),
        "number,title,body,url,state,baseRefName,baseRefOid,headRefOid".to_string(),
    ]
}

fn git_argv(args: &[&str]) -> Vec<String> {
    std::iter::once("git".to_string())
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect()
}

fn gh_output() -> String {
    serde_json::json!({
        "number": 42,
        "title": "Keep every finding",
        "body": "Review intent",
        "url": PULL_REQUEST_URL,
        "state": "OPEN",
        "baseRefName": "main",
        "baseRefOid": "base-oid",
        "headRefOid": "head-oid",
    })
    .to_string()
}

fn expected_metadata() -> PullRequestMetadata {
    PullRequestMetadata {
        number: 42,
        title: "Keep every finding".to_string(),
        body: "Review intent".to_string(),
        url: PULL_REQUEST_URL.to_string(),
        state: "OPEN".to_string(),
        base_ref_name: "main".to_string(),
        base_ref_oid: "base-oid".to_string(),
        head_ref_oid: "head-oid".to_string(),
    }
}

fn response(argv: Vec<String>, exit_code: i32, stdout: &str, stderr: &str) -> FakeResponse {
    FakeResponse {
        argv,
        output: ReviewCommandOutput {
            exit_code,
            stdout: stdout.to_string(),
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

    fn assert_finished(&self) {
        assert!(
            self.responses.lock().expect("response lock").is_empty(),
            "not all expected commands ran"
        );
    }
}

impl ReviewCommandRunner for FakeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        assert_eq!(command.cwd, cwd());
        assert_eq!(command.output_bytes_cap, REVIEW_COMMAND_OUTPUT_BYTES_CAP);
        assert_eq!(
            command.env.get("GIT_TERMINAL_PROMPT"),
            Some(&"0".to_string())
        );
        if command.argv.first().map(String::as_str) == Some("gh") {
            assert_eq!(command.timeout, GH_COMMAND_TIMEOUT);
            assert_eq!(
                command.env.get("GH_PROMPT_DISABLED"),
                Some(&"1".to_string())
            );
        } else {
            assert_eq!(command.timeout, GIT_COMMAND_TIMEOUT);
            assert_eq!(
                command.env.get("GIT_OPTIONAL_LOCKS"),
                Some(&"0".to_string())
            );
        }
        let response = self
            .responses
            .lock()
            .expect("response lock")
            .pop_front()
            .expect("unexpected review command");
        assert_eq!(command.argv, response.argv);
        Ok(response.output)
    }
}
