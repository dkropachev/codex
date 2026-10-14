use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::compact::SUMMARY_PREFIX;
use codex_features::Feature;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
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
            config.features.enable(Feature::HandoffContext).unwrap();
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
    let first_metadata = requests[0].body_json()["client_metadata"].clone();
    let second_metadata = requests[1].body_json()["client_metadata"].clone();
    assert!(first_metadata["turn_id"].is_string());
    assert_eq!(first_metadata["turn_id"], second_metadata["turn_id"]);
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
async fn context_handoff_caps_complete_checkpoint_summary() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let oversized_summary = "x".repeat(/*n*/ 40_000);
    let accepted_summary = "y".repeat(40_000 - SUMMARY_PREFIX.len() - 1);
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_function_call(
                    "oversized",
                    "handoff_context",
                    &json!({"summary": oversized_summary}).to_string(),
                ),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_function_call(
                    "bounded",
                    "handoff_context",
                    &json!({"summary": accepted_summary}).to_string(),
                ),
                ev_completed("resp-2"),
            ]),
            sse(vec![
                ev_response_created("resp-3"),
                ev_assistant_message("msg-3", "done"),
                ev_completed("resp-3"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::HandoffContext).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("Preserve this task until a bounded checkpoint is accepted.")
        .await?;
    let requests = responses.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].body_contains_text("Preserve this task"));
    assert!(
        requests[1].function_call_output("oversized")["output"]
            .as_str()
            .unwrap()
            .contains("including the checkpoint prefix")
    );
    let summaries = requests[2]
        .message_input_texts("user")
        .into_iter()
        .filter(|text| text.starts_with(SUMMARY_PREFIX))
        .collect::<Vec<_>>();
    assert_eq!(
        summaries,
        vec![format!("{SUMMARY_PREFIX}\n{accepted_summary}")]
    );
    assert_eq!(
        codex_utils_output_truncation::approx_token_count(&summaries[0]),
        10_000
    );
    assert!(!requests[2].body_contains_text("Preserve this task"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_cancelled_summary_does_not_replace_later_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let interrupted = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("interrupted"),
            ev_function_call(
                "old-handoff",
                "handoff_context",
                &json!({"summary": "STALE_CHECKPOINT"}).to_string(),
            ),
            ev_function_call(
                "pause",
                "request_user_input",
                &json!({"questions": [{
                    "id": "continue", "header": "Continue", "question": "Continue?",
                    "options": [
                        {"label": "Yes", "description": "Proceed"},
                        {"label": "No", "description": "Stop"}
                    ]
                }]})
                .to_string(),
            ),
            ev_completed("interrupted"),
        ]),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.features.enable(Feature::HandoffContext).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "prepare the original work".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: test.session_configured.model.clone(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::RequestUserInput(_))
    })
    .await;
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert_eq!(interrupted.requests().len(), 1);
    let recovery = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("recovery"),
                ev_function_call("invalid-handoff", "handoff_context", r#"{"summary":""}"#),
                ev_completed("recovery"),
            ]),
            sse(vec![
                ev_assistant_message("done", "Recovered."),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    test.submit_turn("NEW_USER_REQUEST must remain in context")
        .await?;
    let requests = recovery.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].body_contains_text("NEW_USER_REQUEST"));
    assert!(
        requests[1]
            .function_call_output("invalid-handoff")
            .is_object()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_tool_requires_independent_opt_in() -> Result<()> {
    skip_if_no_network!(Ok(()));
    for enabled in [false, true] {
        let server = start_mock_server().await;
        let response = mount_sse_once(
            &server,
            sse(vec![
                ev_assistant_message("done", "Ready."),
                ev_completed("done"),
            ]),
        )
        .await;
        let test = test_codex()
            .with_config(move |config| {
                config.features.disable(Feature::TokenBudget).unwrap();
                config.features.disable(Feature::HandoffContext).unwrap();
                if enabled {
                    config.features.enable(Feature::HandoffContext).unwrap();
                }
            })
            .build_with_auto_env(&server)
            .await?;
        test.submit_turn("Inspect available tools.").await?;
        let body = response.single_request().body_json();
        let visible = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "handoff_context");
        assert_eq!(visible, enabled);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_retains_live_process() -> Result<()> {
    skip_if_no_network!(Ok(()));
    core_test_support::skip_if_target_windows!(Ok(()), "uses the POSIX cat command");
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(
                    "open",
                    "exec_command",
                    r#"{"cmd":"cat","tty":true,"yield_time_ms":1000}"#,
                ),
                ev_completed("open"),
            ]),
            sse(vec![
                ev_function_call(
                    "checkpoint",
                    "handoff_context",
                    r#"{"summary":"Continue using the live process."}"#,
                ),
                ev_completed("checkpoint"),
            ]),
            sse(vec![
                ev_function_call(
                    "poll",
                    "write_stdin",
                    r#"{"session_id":1000,"chars":"RETAINED_PROCESS\n","yield_time_ms":1000}"#,
                ),
                ev_completed("poll"),
            ]),
            sse(vec![
                ev_assistant_message("done", "Done."),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_model("gpt-5.5")
        .with_config(|config| {
            config.features.enable(Feature::HandoffContext).unwrap();
            config.features.enable(Feature::UnifiedExec).unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("Keep a live process through a checkpoint.")
        .await?;
    let requests = responses.requests();
    assert_eq!(requests.len(), 4);
    let inventory = requests[2]
        .message_input_texts("developer")
        .into_iter()
        .find(|text| text.starts_with("<context_handoff_live_handles>"))
        .expect("live handle inventory");
    assert_eq!(
        inventory,
        "<context_handoff_live_handles>process 1000</context_handoff_live_handles>"
    );
    assert!(
        requests[3].function_call_output("poll")["output"]
            .as_str()
            .unwrap()
            .contains("RETAINED_PROCESS")
    );
    Ok(())
}
