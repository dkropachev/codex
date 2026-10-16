//! Exercises real local spawning and residency eviction with a mocked model.
use super::*;
use assert_matches::assert_matches;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceSpec;
use codex_protocol::dynamic_tools::DynamicToolNamespaceTool;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;

async fn terminal_or_tool_event(thread: &CodexThread) -> EventMsg {
    timeout(Duration::from_secs(30), async {
        loop {
            let event = thread
                .next_event()
                .await
                .expect("test fixture operation should succeed")
                .msg;
            match event {
                EventMsg::DynamicToolCallRequest(_) | EventMsg::TurnComplete(_) => return event,
                EventMsg::Error(error) => panic!("thread failed: {error:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("thread should reach tool request or completion")
}

async fn spawn_fresh(
    harness: &AgentControlHarness,
    parent: ThreadId,
    depth: i32,
    prompt: &str,
) -> Arc<CodexThread> {
    let (child, _) = harness
        .control
        .spawn(SpawnRequest {
            caller: parent,
            config: harness.config.clone(),
            input: AgentInput::UserInput(text_input(prompt)),
            source: SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: parent,
                depth,
                agent_path: None,
                agent_nickname: None,
                agent_role: None,
            }),
            options: SpawnAgentOptions {
                parent_thread_id: Some(parent),
                ..Default::default()
            },
        })
        .await
        .expect("fresh local spawn should succeed");
    harness
        .manager
        .get_thread(child.thread_id)
        .await
        .expect("test fixture operation should succeed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_child_inherits_tools_when_reservation_evicts_idle_parent() {
    let server = responses::start_mock_server().await;
    let parent_request = responses::mount_sse_once_match(
        &server,
        |r: &wiremock::Request| String::from_utf8_lossy(&r.body).contains("parent-only-context"),
        responses::sse(vec![
            responses::ev_response_created("parent"),
            responses::ev_assistant_message("parent-msg", "Parent finished."),
            responses::ev_completed("parent"),
        ]),
    )
    .await;
    let worker_request = responses::mount_sse_once_match(
        &server,
        |r: &wiremock::Request| String::from_utf8_lossy(&r.body).contains("hold-worker-active"),
        responses::sse(vec![
            responses::ev_response_created("worker"),
            responses::ev_function_call("worker-gate", "client_echo", "{\"message\":\"hold\"}"),
            responses::ev_completed("worker"),
        ]),
    )
    .await;
    let child_request = responses::mount_sse_once_match(
        &server,
        |r: &wiremock::Request| {
            let body = String::from_utf8_lossy(&r.body);
            body.contains("fresh-child-input") && !body.contains("child-echo")
        },
        responses::sse(vec![
            responses::ev_response_created("child"),
            responses::ev_function_call(
                "child-echo",
                "client_echo",
                "{\"message\":\"child-input\"}",
            ),
            responses::ev_completed("child"),
        ]),
    )
    .await;
    let continuation = responses::mount_sse_once_match(
        &server,
        |r: &wiremock::Request| String::from_utf8_lossy(&r.body).contains("child-echo"),
        responses::sse(vec![
            responses::ev_response_created("child-done"),
            responses::ev_assistant_message("child-msg", "Received echo result."),
            responses::ev_completed("child-done"),
        ]),
    )
    .await;
    let (home, mut config) = test_config().await;
    for feature in [Feature::MultiAgentV2, Feature::MultiAgentV2DynamicTools] {
        config
            .features
            .enable(feature)
            .expect("test fixture operation should succeed");
    }
    config
        .features
        .disable(Feature::EnableRequestCompression)
        .expect("test fixture operation should succeed");
    config.model_provider.base_url = Some(format!("{}/v1", server.uri()));
    // Root is excluded, leaving two slots occupied by P and W.
    config.multi_agent_v2.max_concurrent_threads_per_session = 3;
    let harness = AgentControlHarness::new_with_config(home, config).await;
    let echo = DynamicToolFunctionSpec {
        name: "client_echo".into(),
        description: "Echo the supplied message through the client.".into(),
        input_schema: json!({"type":"object", "properties":{"message":{"type":"string"}}, "required":["message"], "additionalProperties":false}),
        defer_loading: false,
    };
    let tools = vec![
        DynamicToolSpec::Function(echo.clone()),
        DynamicToolSpec::Namespace(DynamicToolNamespaceSpec {
            name: "archive".into(),
            description: "Distinctive inherited namespace.".into(),
            tools: vec![DynamicToolNamespaceTool::Function(
                DynamicToolFunctionSpec {
                    name: "lookup".into(),
                    defer_loading: true,
                    ..echo.clone()
                },
            )],
        }),
    ];
    let root = harness
        .manager
        .start_thread(StartThreadOptions {
            history_mode: Some(ThreadHistoryMode::Paginated),
            dynamic_tools: tools.clone(),
            environments: Some(Vec::new()),
            ..StartThreadOptions::new(harness.config.clone())
        })
        .await
        .expect("test fixture operation should succeed");
    let parent = spawn_fresh(
        &harness,
        root.thread_id,
        /*depth*/ 1,
        "parent-only-context",
    )
    .await;
    assert_matches!(
        terminal_or_tool_event(&parent).await,
        EventMsg::TurnComplete(_)
    );
    assert_eq!(parent.session.dynamic_tools().await, tools);
    assert_matches!(parent.agent_status().await, AgentStatus::Completed(_));
    let worker = spawn_fresh(
        &harness,
        root.thread_id,
        /*depth*/ 1,
        "hold-worker-active",
    )
    .await;
    assert_matches!(
        terminal_or_tool_event(&worker).await,
        EventMsg::DynamicToolCallRequest(_)
    );
    // An unanswered real client request gates W without sleeps or a fabricated active turn.
    assert!(worker.session.active_turn.lock().await.is_some());
    assert_eq!(worker.agent_status().await, AgentStatus::Running);
    timeout(Duration::from_secs(10), async {
        while parent.session.active_turn.lock().await.is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("parent turn should become idle");
    let child = spawn_fresh(
        &harness,
        parent.session.thread_id,
        /*depth*/ 2,
        "fresh-child-input",
    )
    .await;
    let err = harness
        .manager
        .get_thread(parent.session.thread_id)
        .await
        .err()
        .expect("reservation must evict P");
    assert_matches!(err.details(), CodexErrorDetails::ThreadNotFound(id) if *id == parent.session.thread_id);
    assert!(
        harness
            .manager
            .get_thread(worker.session.thread_id)
            .await
            .is_ok()
    );
    assert!(worker.session.active_turn.lock().await.is_some());
    assert_eq!(worker.agent_status().await, AgentStatus::Running);
    assert_eq!(child.session.dynamic_tools().await, tools);
    child.ensure_rollout_materialized().await;
    child
        .flush_rollout()
        .await
        .expect("test fixture operation should succeed");
    let meta = codex_rollout::read_session_meta_line(
        &child
            .rollout_path()
            .expect("test fixture operation should succeed"),
    )
    .await
    .expect("test fixture operation should succeed");
    assert_eq!(meta.meta.dynamic_tools, Some(tools));
    assert_eq!(meta.meta.history_mode, ThreadHistoryMode::Paginated);
    assert_eq!(meta.meta.subagent_history_start_ordinal, None);
    let EventMsg::DynamicToolCallRequest(call) = terminal_or_tool_event(&child).await else {
        panic!("child should call inherited tool");
    };
    assert_eq!(
        (
            call.call_id.as_str(),
            call.namespace.as_deref(),
            call.tool.as_str(),
            &call.arguments
        ),
        (
            "child-echo",
            None,
            "client_echo",
            &json!({"message":"child-input"})
        )
    );
    let request = child_request.single_request();
    assert!(
        !request
            .message_input_texts("user")
            .iter()
            .any(|text| text.contains("parent-only-context"))
    );
    let expected_echo = json!({"type":"function", "name":echo.name, "description":echo.description, "parameters":echo.input_schema, "strict":false});
    let body = request.body_json();
    assert_eq!(
        body["tools"]
            .as_array()
            .expect("test fixture operation should succeed")
            .iter()
            .find(|tool| tool["name"] == "client_echo"),
        Some(&expected_echo)
    );
    child
        .submit(Op::DynamicToolResponse {
            id: call.call_id,
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "eviction-safe-echo-result".into(),
                }],
                success: true,
            },
        })
        .await
        .expect("test fixture operation should succeed");
    assert_matches!(
        terminal_or_tool_event(&child).await,
        EventMsg::TurnComplete(_)
    );
    assert_eq!(
        continuation
            .single_request()
            .function_call_output_content_and_success("child-echo"),
        Some((Some("eviction-safe-echo-result".into()), None))
    );
    assert_eq!(parent_request.requests().len(), 1);
    assert_eq!(worker_request.requests().len(), 1);
    worker
        .shutdown_and_wait()
        .await
        .expect("test fixture operation should succeed");
    child
        .shutdown_and_wait()
        .await
        .expect("test fixture operation should succeed");
    root.thread
        .shutdown_and_wait()
        .await
        .expect("test fixture operation should succeed");
}
