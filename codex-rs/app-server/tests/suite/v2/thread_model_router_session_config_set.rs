use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::to_response;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadModelRouterSessionConfigSetParams;
use codex_app_server_protocol::ThreadModelRouterSessionConfigSetResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use std::path::Path;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_model_router_session_config_set_accepts_loaded_thread() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        /*with_model_router*/ true,
    )?;

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
        .send_thread_model_router_session_config_set_request(
            ThreadModelRouterSessionConfigSetParams {
                thread_id: thread.id,
                enabled: Some(false),
            },
        )
        .await?;
    let set_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(set_id)),
    )
    .await??;
    let _: ThreadModelRouterSessionConfigSetResponse =
        to_response::<ThreadModelRouterSessionConfigSetResponse>(set_resp)?;

    Ok(())
}

#[tokio::test]
async fn thread_model_router_session_config_set_rejects_enable_without_configured_router()
-> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        &server.uri(),
        /*with_model_router*/ false,
    )?;

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
        .send_thread_model_router_session_config_set_request(
            ThreadModelRouterSessionConfigSetParams {
                thread_id: thread.id,
                enabled: Some(true),
            },
        )
        .await?;
    let JSONRPCError { error, .. } = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(set_id)),
    )
    .await??;
    assert!(
        error.message.contains(
            "cannot enable model router for this session because no [model_router] is configured"
        ),
        "unexpected error: {error:?}"
    );

    Ok(())
}

fn create_config_toml(
    codex_home: &Path,
    server_uri: &str,
    with_model_router: bool,
) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    let model_router = if with_model_router {
        r#"
[model_router]
enabled = true

[[model_router.candidates]]
model = "gpt-5.4"
"#
    } else {
        ""
    };
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
{model_router}
"#
        ),
    )
}
