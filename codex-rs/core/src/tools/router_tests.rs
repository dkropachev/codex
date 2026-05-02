use std::collections::HashSet;
use std::sync::Arc;

use crate::session::tests::make_session_and_context;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolCallSource;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::registry::ToolRegistry;
use crate::tools::router_index::ToolRouterIndex;
use crate::turn_diff_tracker::TurnDiffTracker;
use codex_state::ToolRouterDiagnosticsWindow;
use codex_state::ToolRouterRequestShape;
use codex_protocol::models::ResponseItem;
use codex_tools::ConfiguredToolSpec;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::ToolCall;
use super::ToolRouter;
use super::ToolRouterParams;
use super::ToolRouterPromptInfo;
use super::ToolRouterTokenEstimates;

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test builds a router from session-owned MCP manager state"
)]
async fn parallel_support_does_not_match_namespaced_local_tool_names() -> anyhow::Result<()> {
    let (session, turn) = make_session_and_context().await;
    let mcp_tools = session
        .services
        .mcp_connection_manager
        .read()
        .await
        .list_all_tools()
        .await;
    let router = ToolRouter::from_config(
        &turn.tools_config,
        ToolRouterParams {
            deferred_mcp_tools: None,
            mcp_tools: Some(mcp_tools),
            unavailable_called_tools: Vec::new(),
            parallel_mcp_server_names: HashSet::new(),
            discoverable_tools: None,
            dynamic_tools: turn.dynamic_tools.as_slice(),
        },
    );

    let parallel_tool_name = ["shell", "local_shell", "exec_command", "shell_command"]
        .into_iter()
        .find(|name| {
            router.tool_supports_parallel(&ToolCall {
                tool_name: ToolName::plain(*name),
                call_id: "call-parallel-tool".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
            })
        })
        .expect("test session should expose a parallel shell-like tool");

    assert!(!router.tool_supports_parallel(&ToolCall {
        tool_name: ToolName::namespaced("mcp__server__", parallel_tool_name),
        call_id: "call-namespaced-tool".to_string(),
        payload: ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    }));

    Ok(())
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test builds a router from session-owned MCP manager state"
)]
async fn tool_router_fanout_does_not_use_general_parallel_support() -> anyhow::Result<()> {
    let (session, turn) = make_session_and_context().await;
    let mcp_tools = session
        .services
        .mcp_connection_manager
        .read()
        .await
        .list_all_tools()
        .await;
    let router = ToolRouter::from_config(
        &turn.tools_config,
        ToolRouterParams {
            deferred_mcp_tools: None,
            mcp_tools: Some(mcp_tools),
            unavailable_called_tools: Vec::new(),
            parallel_mcp_server_names: HashSet::new(),
            discoverable_tools: None,
            dynamic_tools: turn.dynamic_tools.as_slice(),
        },
    );

    let parallel_tool_name = ["shell", "local_shell", "exec_command", "shell_command"]
        .into_iter()
        .find(|name| {
            router.tool_supports_parallel(&ToolCall {
                tool_name: ToolName::plain(*name),
                call_id: "call-parallel-tool".to_string(),
                payload: ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
            })
        })
        .expect("test session should expose a parallel shell-like tool");
    let call = ToolCall {
        tool_name: ToolName::plain(parallel_tool_name),
        call_id: "call-router-fanout".to_string(),
        payload: ToolPayload::Function {
            arguments: "{}".to_string(),
        },
    };

    assert!(router.tool_supports_parallel(&call));
    assert!(!router.tool_router_fanout_safe(&call));

    Ok(())
}

#[tokio::test]
async fn build_tool_call_uses_namespace_for_registry_name() -> anyhow::Result<()> {
    let (session, _) = make_session_and_context().await;
    let session = Arc::new(session);
    let tool_name = "create_event".to_string();

    let call = ToolRouter::build_tool_call(
        &session,
        ResponseItem::FunctionCall {
            id: None,
            name: tool_name.clone(),
            namespace: Some("mcp__codex_apps__calendar".to_string()),
            arguments: "{}".to_string(),
            call_id: "call-namespace".to_string(),
        },
    )
    .await?
    .expect("function_call should produce a tool call");

    assert_eq!(
        call.tool_name,
        ToolName::namespaced("mcp__codex_apps__calendar", tool_name)
    );
    assert_eq!(call.call_id, "call-namespace");
    match call.payload {
        ToolPayload::Function { arguments } => {
            assert_eq!(arguments, "{}");
        }
        other => panic!("expected function payload, got {other:?}"),
    }

    Ok(())
}

#[tokio::test]
async fn mcp_parallel_support_uses_exact_payload_server() -> anyhow::Result<()> {
    let (_, turn) = make_session_and_context().await;
    let router = ToolRouter::from_config(
        &turn.tools_config,
        ToolRouterParams {
            deferred_mcp_tools: None,
            mcp_tools: None,
            unavailable_called_tools: Vec::new(),
            parallel_mcp_server_names: HashSet::from(["echo".to_string()]),
            discoverable_tools: None,
            dynamic_tools: turn.dynamic_tools.as_slice(),
        },
    );

    let deferred_call = ToolCall {
        tool_name: ToolName::namespaced("mcp__echo__", "query_with_delay"),
        call_id: "call-deferred".to_string(),
        payload: ToolPayload::Mcp {
            server: "echo".to_string(),
            tool: "query_with_delay".to_string(),
            raw_arguments: "{}".to_string(),
        },
    };
    assert!(router.tool_supports_parallel(&deferred_call));

    let different_server_call = ToolCall {
        tool_name: ToolName::namespaced("mcp__hello_echo__", "query_with_delay"),
        call_id: "call-other-server".to_string(),
        payload: ToolPayload::Mcp {
            server: "hello_echo".to_string(),
            tool: "query_with_delay".to_string(),
            raw_arguments: "{}".to_string(),
        },
    };
    assert!(!router.tool_supports_parallel(&different_server_call));

    Ok(())
}

#[tokio::test]
async fn routed_inner_dispatch_records_router_source() -> anyhow::Result<()> {
    let (session, turn) = make_session_and_context().await;
    let recorded_source = Arc::new(Mutex::new(None));
    let tool_name = ToolName::plain("list_dir");
    let specs = vec![ConfiguredToolSpec::new(function_tool("list_dir"), false)];
    let registry = ToolRegistry::with_handler_for_test(
        tool_name,
        Arc::new(RecordingHandler {
            source: Arc::clone(&recorded_source),
        }),
    );
    let index = ToolRouterIndex::build(&specs, &registry, &HashSet::new());
    let router = ToolRouter {
        registry,
        specs,
        index,
        model_visible_specs: Vec::new(),
        parallel_mcp_server_names: HashSet::new(),
        tool_router_token_estimates: None,
        tool_router_prompt_info: None,
    };
    let call = ToolCall {
        tool_name: ToolName::plain("tool_router"),
        call_id: "router-call".to_string(),
        payload: ToolPayload::Function {
            arguments: json!({
                "request": "list",
                "where": {"kind": "workspace"},
                "targets": [{"kind": "path", "path": "."}],
                "action": {"kind": "list", "description": "list"},
                "verbosity": "auto"
            })
            .to_string(),
        },
    };

    let result = router
        .dispatch_tool_call_with_code_mode_result(
            Arc::new(session),
            Arc::new(turn),
            CancellationToken::new(),
            Arc::new(Mutex::new(TurnDiffTracker::new())),
            call,
            ToolCallSource::Direct,
        )
        .await?;

    assert_eq!(result.call_id, "router-call");
    assert_eq!(
        *recorded_source.lock().await,
        Some(ToolCallSource::Routed {
            router_call_id: "router-call".to_string(),
        })
    );

    Ok(())
}

#[tokio::test]
async fn route_errors_record_sanitized_request_shape() -> anyhow::Result<()> {
    let (mut session, turn) = make_session_and_context().await;
    let codex_home = tempfile::tempdir().expect("temp dir");
    let state_db = codex_state::StateRuntime::init(
        codex_home.path().to_path_buf(),
        "openai".to_string(),
    )
    .await?;
    session.services.state_db = Some(Arc::clone(&state_db));
    turn.increment_model_response_ordinal();

    let registry = ToolRegistry::empty_for_test();
    let index = ToolRouterIndex::build(&[], &registry, &HashSet::new());
    let router = ToolRouter {
        registry,
        specs: Vec::new(),
        index,
        model_visible_specs: Vec::new(),
        parallel_mcp_server_names: HashSet::new(),
        tool_router_token_estimates: Some(ToolRouterTokenEstimates {
            visible_router_schema_tokens: 10,
            hidden_tool_schema_tokens: 20,
        }),
        tool_router_prompt_info: Some(ToolRouterPromptInfo {
            format_description: String::new(),
            format_description_tokens: 5,
            toolset_hash: "test-toolset".to_string(),
            router_schema_version: 1,
        }),
    };
    let call = ToolCall {
        tool_name: ToolName::plain("tool_router"),
        call_id: "router-call".to_string(),
        payload: ToolPayload::Function {
            arguments: json!({
                "request": "track progress without a plan tool",
                "where": {"kind": "none"},
                "targets": [],
                "action": {"kind": "update_plan", "input": {"plan": []}}
            })
            .to_string(),
        },
    };

    let err = router
        .dispatch_tool_call_with_code_mode_result(
            Arc::new(session),
            Arc::new(turn),
            CancellationToken::new(),
            Arc::new(Mutex::new(TurnDiffTracker::new())),
            call,
            ToolCallSource::Direct,
        )
        .await
        .expect_err("route should fail");
    assert!(err.to_string().contains("could not deterministically route"));

    let observations = state_db
        .tool_router_tune_observations(ToolRouterDiagnosticsWindow::AllTime, None)
        .await?;

    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].invalid_route_errors, 1);
    assert_eq!(
        observations[0].request_shape_clusters,
        vec![codex_state::ToolRouterRequestShapeCluster {
            shape: ToolRouterRequestShape {
                where_kind: "none".to_string(),
                action_kind: "update_plan".to_string(),
                target_kinds: Vec::new(),
                payload_fields: vec!["input".to_string()],
            },
            route_kind: "error".to_string(),
            outcome: Some("route_error".to_string()),
            count: 1,
        }]
    );

    Ok(())
}

struct RecordingHandler {
    source: Arc<Mutex<Option<ToolCallSource>>>,
}

impl ToolHandler for RecordingHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<FunctionToolOutput, crate::function_tool::FunctionCallError> {
        *self.source.lock().await = Some(invocation.source);
        Ok(FunctionToolOutput::from_text("ok".to_string(), Some(true)))
    }
}

fn function_tool(name: &str) -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: name.to_string(),
        description: String::new(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(Default::default(), None, Some(false.into())),
        output_schema: None,
    })
}
