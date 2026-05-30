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
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput as V2UserInput;
use core_test_support::responses;
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
async fn thread_model_router_session_config_set_controls_next_turn_routing() -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_assistant_message("msg-1", "first"),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-2"),
                responses::ev_assistant_message("msg-2", "second"),
                responses::ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-3"),
                responses::ev_assistant_message("msg-3", "third"),
                responses::ev_completed("resp-3"),
            ]),
        ],
    )
    .await;

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

    start_turn(&mut mcp, thread.id.clone(), "first").await?;

    let enable_id = mcp
        .send_thread_model_router_session_config_set_request(
            ThreadModelRouterSessionConfigSetParams {
                thread_id: thread.id.clone(),
                enabled: Some(true),
            },
        )
        .await?;
    let enable_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(enable_id)),
    )
    .await??;
    let _: ThreadModelRouterSessionConfigSetResponse =
        to_response::<ThreadModelRouterSessionConfigSetResponse>(enable_resp)?;
    start_turn(&mut mcp, thread.id.clone(), "second").await?;

    let inherit_id = mcp
        .send_thread_model_router_session_config_set_request(
            ThreadModelRouterSessionConfigSetParams {
                thread_id: thread.id.clone(),
                enabled: None,
            },
        )
        .await?;
    let inherit_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(inherit_id)),
    )
    .await??;
    let _: ThreadModelRouterSessionConfigSetResponse =
        to_response::<ThreadModelRouterSessionConfigSetResponse>(inherit_resp)?;
    start_turn(&mut mcp, thread.id.clone(), "third").await?;

    let requests = response_mock.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].body_json()["model"].as_str(),
        Some("mock-model")
    );
    assert_eq!(
        requests[1].body_json()["model"].as_str(),
        Some("mock-routed-model")
    );
    assert_eq!(
        requests[2].body_json()["model"].as_str(),
        Some("mock-model")
    );

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

async fn start_turn(mcp: &mut McpProcess, thread_id: String, text: &str) -> Result<()> {
    let turn_id = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id,
            input: vec![V2UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let turn_resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(turn_id)),
    )
    .await??;
    let _: TurnStartResponse = to_response::<TurnStartResponse>(turn_resp)?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
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
enabled = false

[model_router.lifecycle.defaults]
shadow_allowed = false

[[model_router.candidates]]
id = "chat-routed"
model = "mock-routed-model"
model_provider = "mock_provider"

[[model_router.models.rules]]
id = "chat-default-routed"
type = "require"
tasks = ["chat.default"]
models = [{ provider = "mock_provider", model = "mock-routed-model" }]
"#
    } else {
        ""
    };
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
model_provider = "mock_provider"
approval_policy = "never"

[model_providers.mock_provider]
name = "Mock provider"
base_url = "{server_uri}/v1"
request_max_retries = 0
stream_max_retries = 0
{model_router}
"#
        ),
    )
}
