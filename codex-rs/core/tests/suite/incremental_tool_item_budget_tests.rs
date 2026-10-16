//! Whole-item budgets across real ordinary MCP catalog refreshes.

use anyhow::Result;
use codex_config::config_toml::RealtimeWsVersion;
use codex_core::ConfigRefreshOutcome;
use codex_extension_api::ToolPolicy;
use codex_features::Feature;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RealtimeConversationVersion;
use codex_protocol::protocol::RealtimeOutputModality;
use codex_thread_store::LoadThreadHistoryParams;
use codex_tools::ToolName;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_mcp_server;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

async fn mcp_server(instructions: String, description: &str) -> MockServer {
    let server = MockServer::start().await;
    let description = description.to_string();
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).expect("MCP request");
            let result = match body["method"].as_str().expect("method") {
                "initialize" => json!({
                    "protocolVersion": body["params"]["protocolVersion"],
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "sample", "version": "1"},
                    "instructions": instructions,
                }),
                "tools/list" => json!({"tools": [{
                    "name": "lookup", "description": description,
                    "inputSchema": {"type": "object", "properties": {}},
                }]}),
                "tools/call" => json!({"content": [{"type": "text", "text": "ready"}]}),
                "notifications/initialized" => return ResponseTemplate::new(/*s*/ 202),
                _ => {
                    return ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                        "jsonrpc": "2.0", "id": body["id"],
                        "error": {"code": -32601, "message": "unsupported"},
                    }));
                }
            };
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "jsonrpc": "2.0", "id": body["id"], "result": result,
            }))
        })
        .mount(&server)
        .await;
    server
}

fn server_config(server: &MockServer) -> codex_config::types::McpServerConfig {
    serde_json::from_value(json!({"url": format!("{}/mcp", server.uri())}))
        .expect("ordinary MCP config")
}

enum ResponsesTransport {
    Http,
    Websocket(String),
}

async fn fixture(
    api: &MockServer,
    mcp: &MockServer,
    realtime_url: &str,
    transport: ResponsesTransport,
) -> Result<TestCodex> {
    let config = server_config(mcp);
    let realtime_url = realtime_url.to_string();
    let mut test = test_codex()
        .with_model_info_override("gpt-5.5", |model| {
            model.use_responses_lite = true;
            model.supports_search_tool = false;
            model.tool_mode = Some(ToolMode::Direct);
        })
        .with_config(move |config_builder| {
            config_builder
                .features
                .enable(Feature::IncrementalTools)
                .expect("feature");
            config_builder
                .mcp_servers
                .set(std::collections::HashMap::from([(
                    "sample".to_string(),
                    config,
                )]))
                .expect("MCP config");
            config_builder.experimental_realtime_ws_base_url = Some(realtime_url);
            config_builder.experimental_realtime_ws_startup_context = Some(String::new());
            config_builder.realtime.version = RealtimeWsVersion::V3;
        })
        .build_with_auto_env(api)
        .await?;
    let mut options = test.start_thread_options().await;
    if let ResponsesTransport::Websocket(url) = transport {
        options.config.model_provider.base_url = Some(format!("{url}/v1"));
        options.config.model_provider.supports_websockets = true;
    }
    options.thread_extension_init.insert(ToolPolicy {
        allowed_tools: Some(vec![ToolName::namespaced("mcp__sample", "lookup")]),
        ..Default::default()
    });
    test.codex.shutdown_and_wait().await?;
    let thread = test.thread_manager.start_thread(options).await?;
    test.codex = thread.thread;
    test.session_configured = thread.session_configured;
    wait_for_mcp_server(&test.codex, "sample").await?;
    Ok(test)
}

async fn capture(test: &TestCodex, api: &MockServer) -> Result<Vec<Value>> {
    let mock =
        responses::mount_sse_once(api, responses::sse(vec![responses::ev_completed("done")])).await;
    test.submit_text_turn("Continue.").await?;
    Ok(mock.single_request().input())
}

async fn refresh(test: &TestCodex, mcp: &MockServer) -> Result<()> {
    let current = test.codex.config().await;
    let mut next = current.as_ref().clone();
    next.mcp_servers.set(std::collections::HashMap::from([(
        "sample".to_string(),
        server_config(mcp),
    )]))?;
    assert_eq!(
        test.codex.refresh_mcp_config(current, next).await,
        ConfigRefreshOutcome::Published
    );
    let result = test
        .codex
        .call_mcp_tool("sample", "lookup", Some(json!({})), /*meta*/ None)
        .await?;
    assert_ne!(result.is_error, Some(true));
    Ok(())
}

fn assert_bounded(items: &[Value]) {
    for item in items {
        let bytes = serde_json::to_vec(item)
            .expect("serialized final item")
            .len();
        assert!(
            bytes <= 40_000,
            "{} final item has {bytes} bytes",
            item["type"]
        );
    }
}

fn instruction_text(items: &[Value]) -> String {
    items
        .iter()
        .filter(|item| {
            item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                == json!(["tools.namespace_instructions"])
        })
        .map(|item| {
            assert!(serde_json::to_vec(item).expect("final text item").len() <= 4_000);
            let text = item["content"][0]["text"]
                .as_str()
                .expect("tool instruction text");
            text.split_once(" UTF-8 bytes):\n")
                .map_or(text, |(_, payload)| payload)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_member_refresh_bounds_final_tool_item() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let api = responses::start_mock_server().await;
    let instructions = "x".repeat(/*n*/ 39_700);
    let initial_mcp = mcp_server(instructions.clone(), "a").await;
    let changed_mcp = mcp_server(instructions.clone(), "b").await;
    let test = fixture(
        &api,
        &initial_mcp,
        "ws://127.0.0.1:1",
        ResponsesTransport::Http,
    )
    .await?;
    let initial = capture(&test, &api).await?;
    let declarations = initial
        .iter()
        .filter(|item| item["type"] == "additional_tools")
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 1);
    assert_eq!(
        declarations[0]["tools"],
        json!([{
            "type": "namespace", "name": "mcp__sample", "description": instructions,
            "tools": [{"type": "function", "name": "lookup", "description": "a",
                "strict": false, "parameters": {"type": "object", "properties": {}}}],
        }])
    );
    assert_eq!(serde_json::to_vec(declarations[0])?.len(), 39_988);
    refresh(&test, &changed_mcp).await?;
    let changed = capture(&test, &api).await?;
    assert_eq!(&changed[..initial.len()], initial.as_slice());
    assert_bounded(&changed[initial.len()..]);
    let delta = &changed[initial.len()..];
    let declaration = delta
        .iter()
        .find(|item| item["type"] == "additional_tools")
        .expect("changed callable declaration");
    let mut expected = declarations[0]["tools"].clone();
    expected[0]["tools"][0]["description"] = json!("b");
    assert_eq!(declaration["tools"], expected);
    assert_eq!(
        instruction_text(delta),
        "For the incremental declaration of the mcp__sample namespace:\nThis is an incremental namespace update. Previously declared tools remain available for direct calls unless explicitly marked unavailable. If a tool is redefined here, its latest definition replaces the earlier one."
    );
    let unchanged = capture(&test, &api).await?;
    assert_eq!(&unchanged[..changed.len()], changed.as_slice());
    assert_eq!(instruction_text(&unchanged[changed.len()..]), "");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_metadata_refresh_does_not_merge_with_realtime() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let api = responses::start_mock_server().await;
    let realtime = responses::start_websocket_server_with_headers(vec![
        responses::WebSocketConnectionConfig {
            requests: vec![vec![json!({
                "type": "session.started", "session": {"id": "budget", "instructions": "backend"},
            })]],
            response_headers: Vec::new(),
            accept_delay: None,
            close_after_requests: false,
        },
    ])
    .await;
    let initial_mcp = mcp_server("old".to_string(), "a").await;
    let changed_mcp = mcp_server("x".repeat(/*n*/ 20_000), "a").await;
    let test = fixture(&api, &initial_mcp, realtime.uri(), ResponsesTransport::Http).await?;
    let initial = capture(&test, &api).await?;
    refresh(&test, &changed_mcp).await?;
    test.codex
        .submit(Op::RealtimeConversationStart(ConversationStartParams {
            client_managed_handoffs: false,
            delegation_ack_filler: None,
            flush_transcript_tail_on_session_end: false,
            codex_responses_as_items: false,
            codex_response_item_prefix: None,
            codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
            backend_reasoning_status: false,
            codex_response_handoff_channel_prefixes: None,
            model: None,
            output_modality: RealtimeOutputModality::Audio,
            include_startup_context: false,
            initial_items: Vec::new(),
            realtime_start_instructions: Some("r".repeat(/*n*/ 25_000)),
            realtime_end_instructions: None,
            prompt: Some(Some("backend".to_string())),
            realtime_session_id: None,
            transport: None,
            version: Some(RealtimeConversationVersion::V3),
            voice: None,
        }))
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::RealtimeConversationStarted(_))
    })
    .await;
    let changed = capture(&test, &api).await?;
    assert_eq!(&changed[..initial.len()], initial.as_slice());
    assert_bounded(&changed[initial.len()..]);
    assert_eq!(
        instruction_text(&changed[initial.len()..]),
        format!(
            "Updated instructions for the mcp__sample namespace:\n{}",
            "x".repeat(/*n*/ 20_000)
        )
    );
    assert!(
        changed[initial.len()..]
            .iter()
            .any(|item| item["content"][0]["text"]
                == format!(
                    "<realtime_conversation>\n{}\n</realtime_conversation>",
                    "r".repeat(/*n*/ 25_000)
                ))
    );
    assert!(
        !changed[initial.len()..]
            .iter()
            .any(|item| item["type"] == "additional_tools")
    );
    test.codex.submit(Op::RealtimeConversationClose).await?;
    realtime.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_metadata_continuations_preserve_large_escaped_instructions_on_resume()
-> Result<()> {
    skip_if_no_network!(Ok(()));
    let api = responses::start_mock_server().await;
    let instructions = format!("{}é🦀\\\"\0\nend", "x".repeat(/*n*/ 80_000));
    let initial_mcp = mcp_server("old".to_string(), "a").await;
    let changed_mcp = mcp_server(instructions.clone(), "a").await;
    let mut test = fixture(
        &api,
        &initial_mcp,
        "ws://127.0.0.1:1",
        ResponsesTransport::Http,
    )
    .await?;
    let initial = capture(&test, &api).await?;
    refresh(&test, &changed_mcp).await?;
    let changed = capture(&test, &api).await?;
    assert_eq!(&changed[..initial.len()], initial.as_slice());
    assert_bounded(&changed[initial.len()..]);
    assert_eq!(
        instruction_text(&changed[initial.len()..]),
        format!("Updated instructions for the mcp__sample namespace:\n{instructions}")
    );
    assert!(
        !changed[initial.len()..]
            .iter()
            .any(|item| item["type"] == "additional_tools")
    );
    let mut options = test.start_thread_options().await;
    options.config = test.codex.config().await.as_ref().clone();
    options.thread_extension_init.insert(ToolPolicy {
        allowed_tools: Some(vec![ToolName::namespaced("mcp__sample", "lookup")]),
        ..Default::default()
    });
    test.codex.shutdown_and_wait().await?;
    let history = test
        .thread_store
        .load_latest_model_context(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    options.initial_history = InitialHistory::Resumed(ResumedHistory {
        history_revision: None,
        conversation_id: history.thread_id,
        history: Arc::new(history.items),
        rollout_path: test.session_configured.rollout_path.clone(),
    });
    let resumed = test.thread_manager.start_thread(options).await?;
    test.codex = resumed.thread;
    test.session_configured = resumed.session_configured;
    let resumed = capture(&test, &api).await?;
    assert_eq!(&resumed[..changed.len()], changed.as_slice());
    assert_eq!(instruction_text(&resumed[changed.len()..]), "");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_metadata_continuations_keep_websocket_connection_and_ids() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let api = responses::start_mock_server().await;
    let websocket = responses::start_websocket_server(vec![
        (0..4)
            .map(|index| vec![responses::ev_completed(&format!("ws-{index}"))])
            .collect(),
    ])
    .await;
    let initial_mcp = mcp_server("old".to_string(), "a").await;
    let instructions = "x".repeat(/*n*/ 80_000);
    let changed_mcp = mcp_server(instructions.clone(), "a").await;
    let test = fixture(
        &api,
        &initial_mcp,
        "ws://127.0.0.1:1",
        ResponsesTransport::Websocket(websocket.uri().to_string()),
    )
    .await?;
    test.submit_text_turn("Start.").await?;
    refresh(&test, &changed_mcp).await?;
    test.submit_text_turn("Refresh.").await?;
    test.submit_text_turn("Unchanged.").await?;
    let connection = websocket.single_connection();
    let requests = connection
        .iter()
        .map(core_test_support::responses::WebSocketRequest::body_json)
        .filter(|request| request["generate"] != false)
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    let initial = requests[0]["input"].as_array().expect("initial input");
    let changed = requests[1]["input"].as_array().expect("delta input");
    assert_bounded(changed);
    assert_eq!(
        instruction_text(changed),
        format!("Updated instructions for the mcp__sample namespace:\n{instructions}")
    );
    let unchanged = requests[2]["input"].as_array().expect("unchanged input");
    if requests[1]["previous_response_id"].is_null() {
        assert_eq!(&changed[..initial.len()], initial.as_slice());
    }
    if requests[2]["previous_response_id"].is_null() {
        assert_eq!(&unchanged[..changed.len()], changed.as_slice());
    } else {
        assert_eq!(instruction_text(unchanged), "");
    }
    assert_eq!(websocket.handshakes().len(), 1);
    websocket.shutdown().await;
    Ok(())
}
