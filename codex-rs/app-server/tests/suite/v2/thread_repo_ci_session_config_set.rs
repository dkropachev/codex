use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RepoCiIssueType;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadRepoCiSessionConfigSetParams;
use codex_app_server_protocol::ThreadRepoCiSessionConfigSetResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_repo_ci_session_config_set_accepts_loaded_thread() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(codex_home.path(), &server.uri())?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let start_id = mcp
        .send_thread_start_request(ThreadStartParams {
            model: Some("mock-model".to_string()),
            ..Default::default()
        })
        .await?;
    let start_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start_id)),
    )
    .await??;
    let ThreadStartResponse { thread, .. } = to_response::<ThreadStartResponse>(start_resp)?;

    let set_id = mcp
        .send_thread_repo_ci_session_config_set_request(ThreadRepoCiSessionConfigSetParams {
            thread_id: thread.id,
            mode: None,
            issue_types: Some(Some(vec![
                RepoCiIssueType::Correctness,
                RepoCiIssueType::Security,
            ])),
            review_rounds: Some(Some(3)),
            long_ci: Some(Some(true)),
        })
        .await?;
    let set_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(set_id)),
    )
    .await??;
    let _: ThreadRepoCiSessionConfigSetResponse =
        to_response::<ThreadRepoCiSessionConfigSetResponse>(set_resp)?;

    Ok(())
}

fn create_config_toml(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "never"

[model_providers.mock_provider]
name = "Mock provider"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
"#
        ),
    )
}
