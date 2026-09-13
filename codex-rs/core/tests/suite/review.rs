use codex_config::config_toml::ModelPolicyReasoningEffortToml;
use codex_config::config_toml::ModelPolicyRouteToml;
use codex_config::config_toml::ModelPolicyRuleToml;
use codex_config::config_toml::ModelPolicyToml;
use codex_core::CodexThread;
use codex_core::REVIEW_PROMPT;
use codex_core::config::Config;
use codex_features::Feature;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExitedReviewModeEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::review_format::render_review_output_text;
use codex_protocol::user_input::UserInput;
use core_test_support::PathBufExt;
use core_test_support::responses;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use uuid::Uuid;
use wiremock::MockServer;

/// Verify that submitting `Op::Review` emits review item lifecycle,
/// legacy review events, and TurnComplete when the model returns a structured review payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_op_emits_lifecycle_and_review_output() {
    // Skip under Codex sandbox network restrictions.
    skip_if_no_network!();

    // Start mock Responses API server. Return a strict discovery payload.
    let review_json = serde_json::json!({
        "candidates": [
            {
                "title": "Prefer Stylize helpers",
                "body": "Use .dim()/.bold() chaining instead of manual Style where possible.",
                "confidenceScore": 0.9,
                "priority": 1,
                "codeLocation": {
                    "absoluteFilePath": "/tmp/file.rs",
                    "lineRange": {"start": 10, "end": 20}
                }
            }
        ],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "All good with some improvements suggested.",
            "confidenceScore": 0.8
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string();
    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    // Submit review request.
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Please review my changes".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    // Item lifecycle events are emitted first, then the legacy review event is fanned out
    // with the same stable IDs for compatibility consumers.
    let entered_started = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemStarted(event)
                if matches!(event.item, TurnItem::EnteredReviewMode(_))
        )
    })
    .await;
    let (review_turn_id, entered_item_id) = match entered_started {
        EventMsg::ItemStarted(event) => (event.turn_id, event.item.id()),
        other => panic!("expected entered review item start, got {other:?}"),
    };
    let entered_completed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemCompleted(event)
                if matches!(event.item, TurnItem::EnteredReviewMode(_))
        )
    })
    .await;
    match entered_completed {
        EventMsg::ItemCompleted(event) => {
            assert_eq!(event.turn_id, review_turn_id);
            assert_eq!(event.item.id(), entered_item_id);
        }
        other => panic!("expected entered review item completion, got {other:?}"),
    }
    let entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    match entered {
        EventMsg::EnteredReviewMode(event) => {
            assert_eq!(event.turn_id.as_deref(), Some(review_turn_id.as_str()));
            assert_eq!(event.item_id.as_deref(), Some(entered_item_id.as_str()));
        }
        other => panic!("expected EnteredReviewMode(..), got {other:?}"),
    }

    let exited_started = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemStarted(event)
                if matches!(event.item, TurnItem::ExitedReviewMode(_))
        )
    })
    .await;
    let exited_item_id = match exited_started {
        EventMsg::ItemStarted(event) => {
            assert_eq!(event.turn_id, review_turn_id);
            event.item.id()
        }
        other => panic!("expected exited review item start, got {other:?}"),
    };
    let exited_completed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ItemCompleted(event)
                if matches!(event.item, TurnItem::ExitedReviewMode(_))
        )
    })
    .await;
    match exited_completed {
        EventMsg::ItemCompleted(event) => {
            assert_eq!(event.turn_id, review_turn_id);
            assert_eq!(event.item.id(), exited_item_id);
        }
        other => panic!("expected exited review item completion, got {other:?}"),
    }
    let closed = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let review = match closed {
        EventMsg::ExitedReviewMode(ev) => {
            assert_eq!(ev.turn_id.as_deref(), Some(review_turn_id.as_str()));
            assert_eq!(ev.item_id.as_deref(), Some(exited_item_id.as_str()));
            ev.review_output
                .expect("expected ExitedReviewMode with Some(review_output)")
        }
        other => panic!("expected ExitedReviewMode(..), got {other:?}"),
    };

    // Deep compare full structure using PartialEq (floats are f32 on both sides).
    let expected = ReviewOutputEvent {
        findings: vec![ReviewFinding {
            title: "Prefer Stylize helpers".to_string(),
            body: "Use .dim()/.bold() chaining instead of manual Style where possible.".to_string(),
            confidence_score: 0.9,
            priority: 1,
            code_location: ReviewCodeLocation {
                absolute_file_path: PathBuf::from("/tmp/file.rs"),
                line_range: ReviewLineRange { start: 10, end: 20 },
            },
            pre_existing: ReviewPreExisting::Undetermined,
            pre_existing_fix_rationale: None,
        }],
        overall_correctness: "patch is incorrect".to_string(),
        overall_explanation: "All good with some improvements suggested.".to_string(),
        overall_confidence_score: 0.8,
        ..Default::default()
    };
    assert_eq!(expected, review);
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let path = codex.rollout_path().expect("rollout path");
    let text = std::fs::read_to_string(&path).expect("read rollout file");
    let parent_thread_id = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| {
            let rollout_line: RolloutLine = serde_json::from_str(line).expect("rollout line");
            match rollout_line.item {
                RolloutItem::SessionMeta(session_meta) => Some(session_meta.meta.id.to_string()),
                _ => None,
            }
        })
        .expect("parent session meta");

    let request = request_log.single_request();
    assert_eq!(
        request.header("x-openai-subagent").as_deref(),
        Some("review")
    );
    let turn_metadata: serde_json::Value = serde_json::from_str(
        &request
            .header("x-codex-turn-metadata")
            .expect("review request turn metadata"),
    )
    .expect("review request turn metadata json");
    assert!(turn_metadata.get("forked_from_thread_id").is_none());
    assert_eq!(
        turn_metadata["parent_thread_id"].as_str(),
        Some(parent_thread_id.as_str())
    );

    // The report is persisted as review output but is not inserted into parent model history yet.
    let expected_assistant_text = render_review_output_text(&expected);
    let mut saw_model_visible_report = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
        let rl: RolloutLine = serde_json::from_value(v).expect("rollout line");
        if let RolloutItem::ResponseItem(ResponseItem::Message { content, .. }) = rl.item {
            for content in content {
                if let ContentItem::InputText { text } | ContentItem::OutputText { text } = content
                    && text.contains(&expected_assistant_text)
                {
                    saw_model_visible_report = true;
                }
            }
        }
    }
    assert!(!saw_model_visible_report);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_review_does_not_forward_delegate_mcp_startup() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let request_log = responses::mount_response_once(
        &server,
        responses::sse_response(responses::sse(vec![responses::ev_response_created(
            "resp-1",
        )]))
        .set_delay(Duration::from_secs(30)),
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    // Consume the parent session's own empty startup round before starting the review.
    wait_for_event(&codex, |event| {
        matches!(event, EventMsg::McpStartupComplete(_))
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Cancel this review".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match codex.next_event().await.expect("review event").msg {
                event @ (EventMsg::McpStartupUpdate(_) | EventMsg::McpStartupComplete(_)) => {
                    panic!("review forwarded delegate MCP startup: {event:?}")
                }
                EventMsg::EnteredReviewMode(_) => break,
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for review entry");

    tokio::time::timeout(Duration::from_secs(5), async {
        while request_log.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("review request did not reach the server");

    codex.submit(Op::Interrupt).await.unwrap();

    let mut exited_review = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match codex
                .next_event()
                .await
                .expect("review cancellation event")
                .msg
            {
                event @ (EventMsg::McpStartupUpdate(_) | EventMsg::McpStartupComplete(_)) => {
                    panic!("cancelled review forwarded delegate MCP startup: {event:?}")
                }
                EventMsg::ExitedReviewMode(ExitedReviewModeEvent { review_output, .. }) => {
                    assert_eq!(review_output, None);
                    exited_review = true;
                }
                EventMsg::TurnAborted(_) if exited_review => break,
                EventMsg::TurnAborted(_) => panic!("review turn aborted before review mode exited"),
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for review cancellation");

    assert_eq!(request_log.requests().len(), 1);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Invalid output is repaired twice, then reported as a structured failure.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_op_with_plain_text_emits_review_fallback() {
    skip_if_no_network!();

    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse("just plain text"),
        /*expected_requests*/ 3,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Plain text review".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let closed = wait_for_event(&codex, |ev| matches!(ev, EventMsg::ExitedReviewMode(_))).await;
    let review = match closed {
        EventMsg::ExitedReviewMode(ev) => ev
            .review_output
            .expect("expected ExitedReviewMode with Some(review_output)"),
        other => panic!("expected ExitedReviewMode(..), got {other:?}"),
    };

    assert_eq!(review.overall_correctness, "uncertain");
    assert!(review.overall_explanation.contains("Review failed"));
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Ensure review flow suppresses assistant-specific streaming/completion events:
/// - AgentMessageContentDelta
/// - ItemCompleted for TurnItem::AgentMessage
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_filters_agent_message_related_events() {
    skip_if_no_network!();

    let (server, _request_log) = start_responses_server_with_sse(
        vec![
            responses::ev_message_item_added("msg-1", ""),
            responses::ev_output_text_delta("Hi"),
            responses::ev_output_text_delta(" there"),
            responses::ev_assistant_message("msg-1", "Hi there"),
            responses::ev_completed("resp-1"),
        ],
        /*expected_requests*/ 3,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Filter streaming events".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let mut saw_entered = false;
    let mut saw_exited = false;

    // Drain until TurnComplete; assert streaming-related events never surface.
    wait_for_event(&codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::EnteredReviewMode(_) => {
            saw_entered = true;
            false
        }
        EventMsg::ExitedReviewMode(_) => {
            saw_exited = true;
            false
        }
        // The following must be filtered by review flow
        EventMsg::AgentMessageContentDelta(_) => {
            panic!("unexpected AgentMessageContentDelta surfaced during review")
        }
        _ => false,
    })
    .await;
    assert!(saw_entered && saw_exited, "missing review lifecycle events");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Structured review output is emitted only through ExitedReviewMode.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_does_not_emit_agent_message_on_structured_output() {
    skip_if_no_network!();

    let review_json = serde_json::json!({
        "candidates": [
            {
                "title": "Example",
                "body": "Structured review output.",
                "confidenceScore": 0.5,
                "priority": 1,
                "codeLocation": {
                    "absoluteFilePath": "/tmp/file.rs",
                    "lineRange": {"start": 1, "end": 2}
                }
            }
        ],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "One issue remains.",
            "confidenceScore": 0.5
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string();
    let (server, _request_log) = start_responses_server_with_sse(
        assistant_message_sse(&review_json),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "check structured".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    // Drain events until TurnComplete; no child assistant message should surface.
    let mut saw_entered = false;
    let mut saw_exited = false;
    let mut agent_messages = 0;
    wait_for_event(&codex, |event| match event {
        EventMsg::TurnComplete(_) => true,
        EventMsg::AgentMessage(_) => {
            agent_messages += 1;
            false
        }
        EventMsg::EnteredReviewMode(_) => {
            saw_entered = true;
            false
        }
        EventMsg::ExitedReviewMode(_) => {
            saw_exited = true;
            false
        }
        _ => false,
    })
    .await;
    assert_eq!(0, agent_messages, "child AgentMessage leaked from review");
    assert!(saw_entered && saw_exited, "missing review lifecycle events");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Ensure that when a custom `review_model` is set in the config, the review
/// request uses that model (and not the main chat model).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_custom_review_model_from_config() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&empty_discovery_json()),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    // Choose a review model different from the main model; ensure it is used.
    let codex = new_conversation_for_server(&server, codex_home.clone(), |cfg| {
        cfg.model = Some("gpt-4.1".to_string());
        cfg.review_model = Some("gpt-5.4".to_string());
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "use custom model".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    // Wait for completion
    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_),
                ..
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Assert the request body model equals the configured review model
    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert_eq!(body["model"].as_str().unwrap(), "gpt-5.4");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_model_is_not_replaced_by_general_model_policy() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&empty_discovery_json()),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |cfg| {
        cfg.model = Some("gpt-4.1".to_string());
        cfg.review_model = Some("gpt-5.4".to_string());
        cfg.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["subagent.review".to_string()]),
                route: ModelPolicyRouteToml {
                    model: Some("gpt-5.2".to_string()),
                    reasoning_effort: Some(ModelPolicyReasoningEffortToml::Low),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "use policy model".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_),
                ..
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert_eq!(body["model"].as_str(), Some("gpt-5.4"));

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// Ensure that when `review_model` is not set in the config, the review request
/// uses the session model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_session_model_when_review_model_unset() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&empty_discovery_json()),
        /*expected_requests*/ 1,
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |cfg| {
        cfg.model = Some("gpt-4.1".to_string());
        cfg.review_model = None;
    })
    .await;

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "use session model".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_),
                ..
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert_eq!(body["model"].as_str().unwrap(), "gpt-4.1");

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// When a review session begins, it must not prepend prior chat history from
/// the parent session. The request `input` should contain only the review
/// prompt from the user.
// Windows CI only: bump to 4 workers to prevent SSE/event starvation and test timeouts.
#[cfg_attr(windows, tokio::test(flavor = "multi_thread", worker_threads = 4))]
#[cfg_attr(not(windows), tokio::test(flavor = "multi_thread", worker_threads = 2))]
async fn review_input_isolated_from_parent_history() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&empty_discovery_json()),
        /*expected_requests*/ 1,
    )
    .await;

    // Seed a parent session history via resume file with both user + assistant items.
    let codex_home = Arc::new(TempDir::new().unwrap());

    let session_file = codex_home.path().join("resume.jsonl");
    {
        let mut f = tokio::fs::File::create(&session_file).await.unwrap();
        let convo_id = Uuid::new_v4();
        // Proper session_meta line (enveloped) with a conversation id
        let meta_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "session_id": convo_id,
                "id": convo_id,
                "timestamp": "2024-01-01T00:00:00Z",
                "cwd": ".",
                "originator": "test_originator",
                "cli_version": "test_version",
                "model_provider": "test-provider"
            }
        });
        f.write_all(format!("{meta_line}\n").as_bytes())
            .await
            .unwrap();

        // Prior user message (enveloped response_item)
        let user = codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::InputText {
                text: "parent: earlier user message".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let user_json = serde_json::to_value(&user).unwrap();
        let user_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:01.000Z",
            "type": "response_item",
            "payload": user_json
        });
        f.write_all(format!("{user_line}\n").as_bytes())
            .await
            .unwrap();

        // Prior assistant message (enveloped response_item)
        let assistant = codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: "parent: assistant reply".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let assistant_json = serde_json::to_value(&assistant).unwrap();
        let assistant_line = serde_json::json!({
            "timestamp": "2024-01-01T00:00:02.000Z",
            "type": "response_item",
            "payload": assistant_json
        });
        f.write_all(format!("{assistant_line}\n").as_bytes())
            .await
            .unwrap();
    }
    let codex = resume_conversation_for_server(
        &server,
        codex_home.clone(),
        session_file.clone(),
        |config| {
            let _ = config.features.enable(Feature::TokenBudget);
        },
    )
    .await;

    // Submit review request; it must start fresh (no parent history in `input`).
    let review_prompt = "Please review only this".to_string();
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: review_prompt.clone(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_),
                ..
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Assert the request contains only isolated review-stage input.
    let request = request_log.single_request();
    assert_eq!(request.path(), "/v1/responses");
    let body = request.body_json();
    assert!(
        !body.to_string().contains("<context_window>"),
        "review child should not inherit token-budget context"
    );
    let tool_names = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>();
    assert!(!tool_names.contains(&"new_context"));
    assert!(!tool_names.contains(&"get_context_remaining"));
    let input = body["input"].as_array().expect("input array");
    assert!(
        !input
            .iter()
            .filter_map(|msg| msg.get("content").and_then(|content| content.as_array()))
            .flat_map(|content| content.iter())
            .filter_map(|entry| entry.get("text").and_then(|text| text.as_str()))
            .any(|text| text.starts_with("<environment_context>"))
    );

    let resolved_review_prompt = format!("Follow these review instructions:\n{review_prompt}");
    let review_text = input
        .iter()
        .filter_map(|msg| msg.get("content").and_then(|content| content.as_array()))
        .flat_map(|content| content.iter())
        .filter_map(|entry| entry.get("text").and_then(|text| text.as_str()))
        .find(|text| text.contains(&resolved_review_prompt))
        .expect("review prompt text");
    assert_eq!(
        review_text,
        format!("<review_target>{resolved_review_prompt}</review_target>"),
        "user message should contain only the resolved target instructions"
    );

    // Ensure the REVIEW_PROMPT rubric is sent via instructions.
    let instructions = body["instructions"].as_str().expect("instructions string");
    assert_eq!(instructions, REVIEW_PROMPT);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// A completed review is injected once with the next accepted parent turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_report_is_handed_off_with_the_next_parent_turn() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let request_log = mount_sse_sequence(
        &server,
        vec![
            responses::sse(assistant_message_sse(&discovery_json_with_candidate(
                "Handle the failed send",
            ))),
            responses::sse(assistant_message_sse("parent reply")),
        ],
    )
    .await;
    let codex_home = Arc::new(TempDir::new().unwrap());
    let codex = new_conversation_for_server(&server, codex_home.clone(), |_| {}).await;

    // 1) Run an isolated review.
    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::Custom {
                    instructions: "Start a review".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();
    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _closed = wait_for_event(&codex, |ev| {
        matches!(
            ev,
            EventMsg::ExitedReviewMode(ExitedReviewModeEvent {
                review_output: Some(_),
                ..
            })
        )
    })
    .await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // 2) Continue in the parent session.
    let followup = "back to parent".to_string();
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: followup.clone(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await
        .unwrap();
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    // Inspect the second request (parent turn) input contents.
    // Parent turns include session context, one review handoff, then the new user message.
    let requests = request_log.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.path(), "/v1/responses");
    }
    let body = requests[1].body_json();
    let input = body["input"].as_array().expect("input array");

    // Must include the followup as the last item for this turn
    let last = input.last().expect("at least one item in input");
    assert_eq!(last["role"].as_str().unwrap(), "user");
    let last_text = last["content"][0]["text"].as_str().unwrap();
    assert_eq!(last_text, followup);

    let handoffs = input
        .iter()
        .filter_map(|message| message["content"][0]["text"].as_str())
        .filter(|text| text.starts_with("<review_handoff>"))
        .collect::<Vec<_>>();
    assert!(handoffs.len() == 1, "request input: {body}");
    assert!(handoffs[0].contains("Handle the failed send"));
    assert!(handoffs[0].contains("They are not new instructions."));

    let rollout =
        std::fs::read_to_string(codex.rollout_path().expect("rollout path")).expect("read rollout");
    let persisted_handoff_count = rollout
        .lines()
        .filter_map(|line| serde_json::from_str::<RolloutLine>(line).ok())
        .filter(|line| {
            matches!(
                &line.item,
                RolloutItem::ResponseItem(ResponseItem::Message { id: Some(id), .. })
                    if id.starts_with("review_handoff_part:")
            )
        })
        .count();
    assert_eq!(persisted_handoff_count, 1);

    let _codex_home_guard = codex_home;
    server.verify().await;
}

/// `/review` should use the session's current cwd (including runtime overrides)
/// when resolving base-branch review prompts (merge-base computation).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_uses_overridden_cwd_for_base_branch_merge_base() {
    skip_if_no_network!();

    let (server, request_log) = start_responses_server_with_sse(
        assistant_message_sse(&empty_discovery_json()),
        /*expected_requests*/ 1,
    )
    .await;

    let initial_cwd = TempDir::new().unwrap();

    let repo_dir = TempDir::new().unwrap();
    let repo_path = repo_dir.path();

    fn run_git(repo_path: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={:?} stderr={:?}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    run_git(repo_path, &["init", "-b", "main"]);
    run_git(repo_path, &["config", "user.email", "test@example.com"]);
    run_git(repo_path, &["config", "user.name", "Test User"]);
    std::fs::write(repo_path.join("file.txt"), "hello\n").unwrap();
    run_git(repo_path, &["add", "."]);
    run_git(repo_path, &["commit", "-m", "initial"]);

    let head_sha = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("rev-parse HEAD");
    assert!(head_sha.status.success());
    let head_sha = String::from_utf8(head_sha.stdout)
        .expect("utf8 sha")
        .trim()
        .to_string();
    run_git(repo_path, &["checkout", "-b", "feature"]);
    std::fs::write(repo_path.join("file.txt"), "hello\nfeature\n").unwrap();
    run_git(repo_path, &["add", "."]);
    run_git(repo_path, &["commit", "-m", "feature"]);

    let codex_home = Arc::new(TempDir::new().unwrap());
    let initial_cwd_path = initial_cwd.path().to_path_buf();
    let codex = new_conversation_for_server(&server, codex_home.clone(), move |config| {
        config.cwd = initial_cwd_path.abs();
    })
    .await;

    core_test_support::submit_thread_settings(
        &codex,
        codex_protocol::protocol::ThreadSettingsOverrides {
            environments: Some(local_selections(repo_path.to_path_buf().abs())),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    codex
        .submit(Op::Review {
            review_request: ReviewRequest {
                target: ReviewTarget::BaseBranch {
                    branch: "main".to_string(),
                },
                verification: Default::default(),
                action: Default::default(),
                user_facing_hint: None,
            },
        })
        .await
        .unwrap();

    let _entered = wait_for_event(&codex, |ev| matches!(ev, EventMsg::EnteredReviewMode(_))).await;
    let _complete = wait_for_event(&codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 1);
    for request in &requests {
        assert_eq!(request.path(), "/v1/responses");
    }
    let body = requests[0].body_json();
    let input = body["input"].as_array().expect("input array");

    let saw_merge_base_sha = input
        .iter()
        .filter_map(|msg| msg["content"][0]["text"].as_str())
        .any(|text| text.contains(&head_sha));
    assert!(
        saw_merge_base_sha,
        "expected review prompt to include merge-base sha {head_sha}"
    );

    let _codex_home_guard = codex_home;
    server.verify().await;
}

fn assistant_message_sse(text: &str) -> Vec<serde_json::Value> {
    vec![
        responses::ev_assistant_message("msg-1", text),
        responses::ev_completed("resp-1"),
    ]
}

fn empty_discovery_json() -> String {
    serde_json::json!({
        "candidates": [],
        "assessment": {
            "verdict": "patch is correct",
            "explanation": "No issues found.",
            "confidenceScore": 1.0
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string()
}

fn discovery_json_with_candidate(title: &str) -> String {
    serde_json::json!({
        "candidates": [{
            "title": title,
            "body": "The failed send is ignored.",
            "confidenceScore": 0.9,
            "priority": 1,
            "codeLocation": {
                "absoluteFilePath": "/tmp/file.rs",
                "lineRange": {"start": 10, "end": 10}
            }
        }],
        "assessment": {
            "verdict": "patch is incorrect",
            "explanation": "One issue remains.",
            "confidenceScore": 0.9
        },
        "reviewContext": [],
        "externalReferences": []
    })
    .to_string()
}

/// Start a mock Responses API server and mount the given SSE events.
async fn start_responses_server_with_sse(
    events: Vec<serde_json::Value>,
    expected_requests: usize,
) -> (MockServer, ResponseMock) {
    let server = start_mock_server().await;
    let sse = responses::sse(events);
    let responses = vec![sse; expected_requests];
    let request_log = mount_sse_sequence(&server, responses).await;
    (server, request_log)
}

/// Create a conversation configured to talk to the provided mock server.
async fn new_conversation_for_server<F>(
    server: &MockServer,
    codex_home: Arc<TempDir>,
    mutator: F,
) -> Arc<CodexThread>
where
    F: FnOnce(&mut Config) + Send + 'static,
{
    let base_url = format!("{}/v1", server.uri());
    let mut builder = test_codex()
        .with_home(codex_home)
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url.clone());
            mutator(config);
        });
    builder
        .build(server)
        .await
        .expect("create conversation")
        .codex
}

/// Create a conversation resuming from a rollout file, configured to talk to the provided mock server.
async fn resume_conversation_for_server<F>(
    server: &MockServer,
    codex_home: Arc<TempDir>,
    resume_path: std::path::PathBuf,
    mutator: F,
) -> Arc<CodexThread>
where
    F: FnOnce(&mut Config) + Send + 'static,
{
    let base_url = format!("{}/v1", server.uri());
    let mut builder = test_codex()
        .with_home(codex_home.clone())
        .with_config(move |config| {
            config.model_provider.base_url = Some(base_url.clone());
            mutator(config);
        });
    builder
        .resume(server, codex_home, resume_path)
        .await
        .expect("resume conversation")
        .codex
}
