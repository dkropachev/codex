use std::path::Path;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use codex_app_server_protocol::ErrorNotification;
use codex_app_server_protocol::JSONRPCNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ReviewAction;
use codex_app_server_protocol::ReviewDelivery;
use codex_app_server_protocol::ReviewStartParams;
use codex_app_server_protocol::ReviewStartResponse;
use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::ReviewVerification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStatus;
use core_test_support::skip_if_remote;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::time::timeout;

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn empty_git_scopes_make_no_model_request() -> Result<()> {
    skip_if_remote!(Ok(()), "creates a host-local Git fixture");

    let fixture = TempDir::new()?;
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    run_git(&workspace, &["init", "--initial-branch=main"])?;
    run_git(&workspace, &["config", "user.email", "test@example.com"])?;
    run_git(&workspace, &["config", "user.name", "Test User"])?;
    run_git(&workspace, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(workspace.join("tracked.txt"), "base\n")?;
    run_git(&workspace, &["add", "tracked.txt"])?;
    run_git(&workspace, &["commit", "-m", "base"])?;
    run_git(&workspace, &["commit", "--allow-empty", "-m", "empty"])?;
    let empty_commit = run_git(&workspace, &["rev-parse", "HEAD"])?;

    let server = create_mock_responses_server_repeating_assistant("unexpected request").await;
    let codex_home = fixture.path().join("codex-home");
    std::fs::create_dir(&codex_home)?;
    super::review::create_config_toml(&codex_home, &server.uri())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .without_auto_env()
        .build()
        .await?;
    timeout(READ_TIMEOUT, app.initialize()).await??;
    let thread_id = start_thread(&mut app, &workspace).await?;

    for target in [
        ReviewTarget::UncommittedChanges,
        ReviewTarget::BaseBranch {
            branch: "main".to_string(),
        },
        ReviewTarget::Commit {
            sha: empty_commit.trim().to_string(),
            title: Some("empty".to_string()),
        },
    ] {
        let request_id = app
            .send_review_start_request(ReviewStartParams {
                thread_id: thread_id.clone(),
                target,
                delivery: Some(ReviewDelivery::Inline),
                verification: Some(ReviewVerification::DoubleCheck),
                action: Some(ReviewAction::Fix),
            })
            .await?;
        let response: JSONRPCResponse = timeout(
            READ_TIMEOUT,
            app.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await??;
        let ReviewStartResponse { turn, .. } = to_response(response)?;
        let notification: JSONRPCNotification = timeout(
            READ_TIMEOUT,
            app.read_stream_until_notification_message("error"),
        )
        .await??;
        let error: ErrorNotification =
            serde_json::from_value(notification.params.context("error notification params")?)?;
        assert_eq!(error.error.message, "Selected review scope has no changes");
        let completed: JSONRPCNotification = timeout(
            READ_TIMEOUT,
            app.read_stream_until_notification_message("turn/completed"),
        )
        .await??;
        let completed: TurnCompletedNotification =
            serde_json::from_value(completed.params.context("completion notification params")?)?;
        assert_eq!(completed.turn.id, turn.id);
        assert_eq!(completed.turn.status, TurnStatus::Failed);
    }

    assert!(
        server
            .received_requests()
            .await
            .context("read mock requests")?
            .is_empty()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn empty_pull_request_scope_makes_no_model_request() -> Result<()> {
    skip_if_remote!(Ok(()), "uses a host-local fake GitHub CLI");

    const URL: &str = "https://github.com/acme/widgets/pull/17";
    let fixture = TempDir::new()?;
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    run_git(&workspace, &["init", "--initial-branch=main"])?;
    run_git(&workspace, &["config", "user.email", "test@example.com"])?;
    run_git(&workspace, &["config", "user.name", "Test User"])?;
    run_git(&workspace, &["config", "commit.gpgsign", "false"])?;
    std::fs::write(workspace.join("tracked.txt"), "base\n")?;
    run_git(&workspace, &["add", "tracked.txt"])?;
    run_git(&workspace, &["commit", "-m", "base"])?;
    let base = git_stdout(&workspace, &["rev-parse", "HEAD"])?;
    run_git(&workspace, &["checkout", "-b", "feature"])?;
    run_git(
        &workspace,
        &["commit", "--allow-empty", "-m", "empty feature"],
    )?;
    let head = git_stdout(&workspace, &["rev-parse", "HEAD"])?;

    let fake_bin = fixture.path().join("fake-bin");
    std::fs::create_dir(&fake_bin)?;
    super::review_pull_request::write_fake_gh(
        &fake_bin,
        &format!(
            r#"#!/bin/sh
set -eu
if [ "$1" != "pr" ] || [ "$2" != "view" ] || [ "$3" != "{URL}" ]; then
  echo "unexpected gh arguments: $*" >&2
  exit 64
fi
printf '%s\n' '{{"number":17,"title":"Empty PR","body":"No tree changes","url":"{URL}","state":"OPEN","baseRefName":"main","baseRefOid":"{base}","headRefOid":"{head}"}}'
"#,
        ),
    )?;
    let path = super::review_pull_request::path_with_prepended_dir(&fake_bin)?;
    let server = create_mock_responses_server_repeating_assistant("unexpected request").await;
    let codex_home = fixture.path().join("codex-home");
    std::fs::create_dir(&codex_home)?;
    super::review::create_config_toml(&codex_home, &server.uri())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(&codex_home)
        .without_auto_env()
        .with_env_overrides(&[("PATH", Some(path.as_str()))])
        .build()
        .await?;
    timeout(READ_TIMEOUT, app.initialize()).await??;
    let thread_id = start_thread(&mut app, &workspace).await?;

    let request_id = app
        .send_review_start_request(ReviewStartParams {
            thread_id,
            target: ReviewTarget::PullRequest {
                url: URL.to_string(),
            },
            delivery: Some(ReviewDelivery::Inline),
            verification: Some(ReviewVerification::DoubleCheck),
            action: Some(ReviewAction::Fix),
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ReviewStartResponse { turn, .. } = to_response(response)?;
    let notification: JSONRPCNotification = timeout(
        READ_TIMEOUT,
        app.read_stream_until_notification_message("error"),
    )
    .await??;
    let error: ErrorNotification =
        serde_json::from_value(notification.params.context("error params")?)?;
    assert_eq!(error.error.message, "Selected review scope has no changes");
    let completed: JSONRPCNotification = timeout(
        READ_TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification =
        serde_json::from_value(completed.params.context("completion params")?)?;
    assert_eq!(completed.turn.id, turn.id);
    assert_eq!(completed.turn.status, TurnStatus::Failed);
    assert!(
        server
            .received_requests()
            .await
            .context("read mock requests")?
            .is_empty()
    );
    Ok(())
}

async fn start_thread(app: &mut TestAppServer, workspace: &Path) -> Result<String> {
    let request_id = app
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            cwd: Some(workspace.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await?;
    let response: JSONRPCResponse = timeout(
        READ_TIMEOUT,
        app.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response(response)?;
    timeout(
        READ_TIMEOUT,
        app.read_stream_until_notification_message("thread/started"),
    )
    .await??;
    Ok(thread.id)
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    Ok(run_git(cwd, args)?.trim().to_string())
}
