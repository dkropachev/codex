use anyhow::Result;
use codex_config::config_toml::ContextHandoffRateToml;
use codex_config::config_toml::ContextHandoffToml;
use codex_core::TurnInputRequest;
use codex_core::config::ContextHandoffConfig;
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
use core_test_support::responses::mount_response_once;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;

fn policy(soft: &str, mid: &str, hard: &str) -> ContextHandoffToml {
    ContextHandoffToml {
        soft_when: soft.to_string(),
        mid_when: mid.to_string(),
        hard_when: hard.to_string(),
        ..Default::default()
    }
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
            config.context_handoff = Some(
                ContextHandoffConfig::parse(&policy(
                    "context.size.tokens >= 0",
                    "context.size.tokens > 1000000000",
                    "context.size.tokens > 1000000000",
                ))
                .unwrap(),
            );
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
async fn context_handoff_resume_preserves_both_guidance_stages() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let setup = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("soft", "Ready."),
                ev_completed("soft"),
            ]),
            sse(vec![
                ev_assistant_message("mid", "Ready."),
                ev_completed("mid"),
            ]),
        ],
    )
    .await;
    let mut initial_builder = test_codex().with_config(|config| {
        config.context_handoff = Some(
            ContextHandoffConfig::parse(&policy(
                "context.size.tokens >= 0",
                "context.size.tokens > 1000000000",
                "context.size.tokens > 1000000000",
            ))
            .unwrap(),
        );
    });
    let initial = initial_builder.build_with_auto_env(&server).await?;
    initial.submit_turn("start").await?;
    let mut mid_builder = test_codex().with_config(|config| {
        config.context_handoff = Some(
            ContextHandoffConfig::parse(&policy(
                "context.size.tokens >= 0",
                "context.size.tokens >= 0",
                "context.size.tokens > 1000000000",
            ))
            .unwrap(),
        );
    });
    let mut mid = mid_builder.restart_with_auto_env(&server, &initial).await?;
    mid.submit_turn("prepare").await?;
    assert_eq!(setup.requests().len(), 2);
    for iteration in 0..2 {
        let response = mount_sse_once(
            &server,
            sse(vec![
                ev_assistant_message("resumed", "Continued."),
                ev_completed("resumed"),
            ]),
        )
        .await;
        let resumed = mid_builder.restart_with_auto_env(&server, &mid).await?;
        resumed
            .submit_turn(&format!("continue {iteration}"))
            .await?;
        let texts = response.single_request().message_input_texts("developer");
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("Consider calling handoff_context"))
                .count(),
            1
        );
        assert_eq!(
            texts
                .iter()
                .filter(|text| text.contains("Prepare to call handoff_context"))
                .count(),
            1
        );
        mid = resumed;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_model_switch_does_not_reuse_cached_price() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mut completed = ev_completed("cached");
    completed["response"]["usage"] = json!({
        "input_tokens": 1000, "input_tokens_details": {"cached_tokens": 1000},
        "output_tokens": 1, "total_tokens": 1001,
    });
    let setup = mount_sse_once(
        &server,
        sse(vec![ev_assistant_message("cached", "Ready."), completed]),
    )
    .await;
    let test = test_codex()
        .with_model("gpt-5.4")
        .with_config(|config| {
            let mut input = policy(
                "context.size.tokens > 1000000000",
                "context.size.tokens > 1000000000",
                "context.size.price >= 100",
            );
            input.rates = Some(BTreeMap::from([(
                "openai/gpt-5.2/default".to_string(),
                ContextHandoffRateToml {
                    input_per_million: 1_000_000.0,
                    cached_input_per_million: 0.0,
                },
            )]));
            config.context_handoff = Some(ContextHandoffConfig::parse(&input).unwrap());
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("populate the old model cache").await?;
    assert_eq!(setup.requests().len(), 1);
    let compact = mount_sse_once(&server, sse(vec![
        json!({"type": "response.output_item.done", "item": {"type": "compaction", "encrypted_content": "checkpoint"}}),
        ev_completed("compact"),
    ])).await;
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "switch models".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                model: Some("gpt-5.2".to_string()),
                ..Default::default()
            }),
        )
        .await?;
    wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(compact.requests().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_guidance_crosses_hard_boundary_before_sampling() -> Result<()> {
    skip_if_no_network!(Ok(()));
    for field in ["tokens", "price"] {
        let server = start_mock_server().await;
        let setup = mount_sse_once(
            &server,
            sse(vec![
                ev_assistant_message("seed", "Ready."),
                ev_completed("seed"),
            ]),
        )
        .await;
        let initial = test_codex()
            .with_config(|config| {
                config.base_instructions = Some("Be helpful.".to_string());
                config.context_handoff = Some(
                    ContextHandoffConfig::parse(&policy(
                        "context.size.tokens > 1000000000",
                        "context.size.tokens > 1000000000",
                        "context.size.tokens > 1000000000",
                    ))
                    .unwrap(),
                );
            })
            .build_with_auto_env(&server)
            .await?;
        initial.submit_turn("start").await?;
        let request = setup.single_request();
        let body = request.body_json();
        let text_tokens = ["developer", "user", "assistant"]
            .into_iter()
            .flat_map(|role| request.message_input_texts(role))
            .map(|text| codex_utils_output_truncation::approx_token_count(&text))
            .sum::<usize>();
        let tool_tokens = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| codex_utils_output_truncation::approx_token_count(&tool.to_string()))
            .sum::<usize>();
        let boundary = text_tokens + tool_tokens + 1000;
        let model = body["model"].as_str().unwrap().to_string();
        let mut builder = test_codex().with_config(move |config| {
            config.base_instructions = Some("Be helpful.".to_string());
            let mut input = policy(
                "context.size.tokens >= 0",
                "context.size.tokens > 1000000000",
                &format!("context.size.{field} >= {boundary}"),
            );
            input.rates = Some(BTreeMap::from([(
                format!("openai/{model}/default"),
                ContextHandoffRateToml {
                    input_per_million: 1_000_000.0,
                    cached_input_per_million: 1_000_000.0,
                },
            )]));
            input.soft_guidance = Some("g".repeat(/*n*/ 4_096));
            input.summary_instructions = Some("s".repeat(/*n*/ 4_096));
            config.context_handoff = Some(ContextHandoffConfig::parse(&input).unwrap());
        });
        let resumed = builder.restart_with_auto_env(&server, &initial).await?;
        let compact = mount_response_once(
            &server,
            wiremock::ResponseTemplate::new(/*s*/ 400).set_body_json(json!({
                "error": {"message": "compaction refused", "type": "invalid_request_error"}
            })),
        )
        .await;
        resumed.submit_turn("continue").await?;
        let request = compact.single_request();
        assert!(
            request.body_contains_text("<handoff_stage>soft</handoff_stage>"),
            "{field} boundary {boundary}"
        );
        let metadata: serde_json::Value = serde_json::from_str(
            request.body_json()["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
        )?;
        assert_eq!(metadata["request_kind"], json!("compaction"));
    }
    Ok(())
}
