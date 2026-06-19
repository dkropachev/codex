use anyhow::Context;
use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadReadParams;
use codex_app_server_protocol::ThreadReadResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadWorkflowCommandParams;
use codex_app_server_protocol::ThreadWorkflowCommandResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_protocol::models::MessagePhase;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const WORKFLOW_MARKDOWN: &str = "# Workflow E2E\n\nmarker=workflow-e2e\n";

#[tokio::test]
async fn thread_workflow_command_records_assistant_output_and_next_turn_context() -> Result<()> {
    let tmp = TempDir::new()?;
    let codex_home = tmp.path().join("codex_home");
    std::fs::create_dir(&codex_home)?;
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workflow_dir = tmp.path().join("workflow");
    std::fs::create_dir(&workflow_dir)?;

    let fake_bin = tmp.path().join("fake_bin");
    std::fs::create_dir(&fake_bin)?;
    write_fake_bun(fake_bin.as_path())?;
    let path_value = path_with_prepended_dir(fake_bin.as_path())?;

    let responses = vec![create_final_assistant_message_sse_response(
        "follow-up answer",
    )?];
    let server = create_mock_responses_server_sequence(responses).await;
    write_mock_responses_config_toml(
        codex_home.as_path(),
        &server.uri(),
        &BTreeMap::default(),
        i64::MAX,
        None,
        "mock_provider",
        "Summarize the conversation.",
    )?;

    let env = [("PATH", Some(path_value.as_str()))];
    let mut mcp = TestAppServer::new_with_env(codex_home.as_path(), &env).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request(ThreadStartParams::default())
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let workflow_id = mcp
        .send_thread_workflow_command_request(ThreadWorkflowCommandParams {
            thread_id: thread.id.clone(),
            workflow_dir: workflow_dir.to_string_lossy().to_string(),
            input: json!({
                "marker": "workflow-e2e",
                "workingDirectory": workspace.to_string_lossy().to_string(),
            }),
        })
        .await?;
    let workflow_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(workflow_id)),
    )
    .await??;
    let _: ThreadWorkflowCommandResponse =
        to_response::<ThreadWorkflowCommandResponse>(workflow_resp)?;

    let started = wait_for_agent_message_started(&mut mcp, WORKFLOW_MARKDOWN).await?;
    assert_agent_message(&started.item, WORKFLOW_MARKDOWN);
    let completed = wait_for_agent_message_completed(&mut mcp, WORKFLOW_MARKDOWN).await?;
    assert_agent_message(&completed.item, WORKFLOW_MARKDOWN);

    let workflow_turn_completed: TurnCompletedNotification = serde_json::from_value(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_message("turn/completed"),
        )
        .await??
        .params
        .context("missing workflow turn/completed params")?,
    )?;
    assert_eq!(workflow_turn_completed.thread_id, thread.id);

    let read_id = mcp
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread.id.clone(),
            include_turns: true,
        })
        .await?;
    let read_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(read_id)),
    )
    .await??;
    let ThreadReadResponse { thread, .. } = to_response::<ThreadReadResponse>(read_resp)?;
    assert_eq!(thread.turns.len(), 1);
    assert!(
        thread.turns[0]
            .items
            .iter()
            .any(|item| agent_message_text(item) == Some(WORKFLOW_MARKDOWN)),
        "thread/read should persist assistant workflow output"
    );

    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: thread.id.clone(),
            client_user_message_id: None,
            input: vec![V2UserInput::Text {
                text: "follow up after workflow".to_string(),
                text_elements: Vec::new(),
            }],
            cwd: Some(workspace),
            ..Default::default()
        })
        .await?;
    let _: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    let requests = server
        .received_requests()
        .await
        .context("failed to fetch mock model requests")?;
    assert_eq!(requests.len(), 1);
    let request_body = requests[0]
        .body_json::<serde_json::Value>()
        .context("model request body should be JSON")?
        .to_string();
    assert!(request_body.contains("follow up after workflow"));
    assert!(request_body.contains("# Workflow E2E"));
    assert!(request_body.contains("marker=workflow-e2e"));

    Ok(())
}

async fn wait_for_agent_message_started(
    mcp: &mut TestAppServer,
    expected_text: &str,
) -> Result<ItemStartedNotification> {
    loop {
        let notif = mcp
            .read_stream_until_notification_message("item/started")
            .await?;
        let started: ItemStartedNotification = serde_json::from_value(
            notif
                .params
                .context("missing item/started notification params")?,
        )?;
        if agent_message_text(&started.item) == Some(expected_text) {
            return Ok(started);
        }
    }
}

async fn wait_for_agent_message_completed(
    mcp: &mut TestAppServer,
    expected_text: &str,
) -> Result<ItemCompletedNotification> {
    loop {
        let notif = mcp
            .read_stream_until_notification_message("item/completed")
            .await?;
        let completed: ItemCompletedNotification = serde_json::from_value(
            notif
                .params
                .context("missing item/completed notification params")?,
        )?;
        if agent_message_text(&completed.item) == Some(expected_text) {
            return Ok(completed);
        }
    }
}

fn assert_agent_message(item: &ThreadItem, expected_text: &str) {
    let ThreadItem::AgentMessage { text, phase, .. } = item else {
        panic!("expected agent message item, got {item:?}");
    };
    assert_eq!(text, expected_text);
    assert_eq!(*phase, Some(MessagePhase::FinalAnswer));
}

fn agent_message_text(item: &ThreadItem) -> Option<&str> {
    match item {
        ThreadItem::AgentMessage { text, .. } => Some(text.as_str()),
        _ => None,
    }
}

fn write_fake_bun(fake_bin: &Path) -> Result<()> {
    let bun_path = fake_bin.join("bun");
    std::fs::write(
        &bun_path,
        format!(
            r#"#!/bin/sh
set -eu
if [ "${{1:-}}" != "--eval" ]; then
  echo "missing --eval" >&2
  exit 64
fi
case "${{2:-}}" in
  *"tui.markdown.v1"*) ;;
  *)
    echo "runner did not request tui.markdown.v1" >&2
    exit 65
    ;;
esac
if [ "${{3:-}}" != "--" ]; then
  echo "missing argument separator" >&2
  exit 66
fi
case "${{4:-}}" in
  *'"marker":"workflow-e2e"'*) ;;
  *)
    echo "workflow input missing marker" >&2
    exit 67
    ;;
esac
cat <<'EOF'
{WORKFLOW_MARKDOWN}EOF
"#
        ),
    )?;
    let mut permissions = std::fs::metadata(&bun_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bun_path, permissions)?;
    Ok(())
}

fn path_with_prepended_dir(dir: &Path) -> Result<String> {
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&existing_path));
    Ok(std::env::join_paths(paths)?.to_string_lossy().to_string())
}
