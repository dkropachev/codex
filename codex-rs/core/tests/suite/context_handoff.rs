use anyhow::Result;
use codex_config::config_toml::ContextHandoffToml;
use codex_core::TurnInputRequest;
use codex_core::config::ContextHandoffConfig;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_handoff_installs_summary_in_same_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "handoff-1",
                    "handoff_context",
                    &json!({"summary": "Continue the user's migration task."}).to_string(),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "Migration complete."),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.context_handoff = Some(
                ContextHandoffConfig::parse(&ContextHandoffToml {
                    soft_when: "context.size.tokens >= 0".to_string(),
                    mid_when: "context.size.tokens > 1000000000".to_string(),
                    hard_when: "context.size.tokens > 1000000000".to_string(),
                    ..Default::default()
                })
                .unwrap(),
            );
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("Begin the user's migration task with a unique old instruction.")
        .await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].body_contains_text("unique old instruction"));
    assert!(requests[1].body_contains_text("Continue the user's migration task."));
    assert!(!requests[1].body_contains_text("unique old instruction"));
    assert!(!requests[1].body_contains_text("handoff-1"));
    for request in &requests {
        assert_eq!(
            request
                .message_input_texts("developer")
                .into_iter()
                .filter(|text| text.contains("Consider calling handoff_context"))
                .count(),
            1,
        );
    }
    let first_body = requests[0].body_json();
    assert!(
        first_body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .any(|name| name == "handoff_context")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hard_stage_stops_when_compaction_cannot_lower_threshold() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let compact = mount_sse_once(
        &server,
        sse(vec![
            json!({
                "type": "response.output_item.done",
                "item": {"type": "compaction", "encrypted_content": "checkpoint"},
            }),
            ev_completed("compact-1"),
        ]),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.context_handoff = Some(
                ContextHandoffConfig::parse(&ContextHandoffToml {
                    soft_when: "context.size.tokens > 1000000000".to_string(),
                    mid_when: "context.size.tokens > 1000000000".to_string(),
                    hard_when: "context.size.tokens >= 0".to_string(),
                    ..Default::default()
                })
                .unwrap(),
            );
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "continue this task".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let message = wait_for_event_match(&test.codex, |event| match event {
        EventMsg::Error(error) => Some(error.message.clone()),
        _ => None,
    })
    .await;
    assert!(message.contains("hard limit still matches after compaction"));
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(compact.requests().len(), 1);
    Ok(())
}
