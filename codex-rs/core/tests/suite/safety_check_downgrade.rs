use anyhow::Result;
use codex_features::Feature;
use codex_protocol::models::ContentItem;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelRerouteReason;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_model_verification_metadata;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_once;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::test_codex::turn_permission_fields;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::ResponseTemplate;

const SERVER_MODEL: &str = "gpt-5.2";
const REQUESTED_MODEL: &str = "gpt-5.3-codex";
const TRUSTED_ACCESS_FOR_CYBER_VERIFICATION: &str = "trusted_access_for_cyber";

const CYBER_POLICY_MESSAGE: &str =
    "This request has been flagged for potentially high-risk cyber activity.";
const FIRST_RECOVERY_PROMPT: &str = "trigger cyber policy recovery";
const SECOND_RECOVERY_PROMPT: &str = "trigger cyber policy recovery again";
const SAFE_RECOVERY_RESPONSE: &str = "I can help with defensive guidance.";
const SECOND_SAFE_RECOVERY_RESPONSE: &str = "I can still help with defensive guidance.";
const CYBER_POLICY_AUTO_RECOVERY_FRAGMENT: &str = concat!(
    "<cyber_policy_auto_recovery>\n",
    "Previous sampling request was blocked by cyber safety checks. Continue the turn ",
    "without repeating the blocked approach. Choose a policy-compliant path that advances ",
    "the user's underlying goal, limiting content to benign, defensive, educational, or ",
    "high-level assistance. Do not provide details that enable harmful activity or attempt ",
    "to bypass safeguards. If no compliant path can complete the request, explain the ",
    "limitation and offer the closest safe alternative.\n",
    "</cyber_policy_auto_recovery>"
);

fn cyber_policy_error_response() -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(serde_json::json!({
        "error": {
            "message": CYBER_POLICY_MESSAGE,
            "type": "invalid_request",
            "param": null,
            "code": "cyber_policy"
        }
    }))
}

fn disabled_text_turn(test: &TestCodex, text: &str) -> Op {
    let (sandbox_policy, permission_profile) =
        turn_permission_fields(PermissionProfile::Disabled, test.cwd_path());
    Op::UserInput {
        items: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        final_output_json_schema: None,
        responsesapi_client_metadata: None,
        additional_context: Default::default(),
        thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
            environments: Some(local_selections(test.config.cwd.clone())),
            approval_policy: Some(AskForApproval::Never),
            sandbox_policy: Some(sandbox_policy),
            permission_profile,
            collaboration_mode: Some(codex_protocol::config_types::CollaborationMode {
                mode: codex_protocol::config_types::ModeKind::Default,
                settings: codex_protocol::config_types::Settings {
                    model: test.session_configured.model.clone(),
                    reasoning_effort: test.config.model_reasoning_effort.clone(),
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        },
    }
}

async fn collect_turn_outcome(test: &TestCodex) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut messages = Vec::new();
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::Error(error) => errors.push(error.message),
            EventMsg::AgentMessage(message) => messages.push(message.message),
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }
    (errors, messages)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_mismatch_emits_warning_event() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response =
        sse_response(sse_completed("resp-1")).insert_header("OpenAI-Model", SERVER_MODEL);
    let _mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger safety check"))
        .await?;

    let reroute = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ModelReroute(_))
    })
    .await;
    let EventMsg::ModelReroute(reroute) = reroute else {
        panic!("expected model reroute event");
    };
    assert_eq!(reroute.from_model, REQUESTED_MODEL);
    assert_eq!(reroute.to_model, SERVER_MODEL);
    assert_eq!(reroute.reason, ModelRerouteReason::HighRiskCyberActivity);

    let warning = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Warning(_))).await;
    let EventMsg::Warning(warning) = warning else {
        panic!("expected warning event");
    };
    assert!(warning.message.contains(REQUESTED_MODEL));
    assert!(warning.message.contains(SERVER_MODEL));

    let _ = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyber_policy_response_emits_typed_error_without_retry() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_response_once(&server, cyber_policy_error_response()).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger cyber policy error"))
        .await?;

    let error = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = error else {
        panic!("expected error event");
    };
    assert_eq!(error.message, CYBER_POLICY_MESSAGE);
    assert_eq!(error.codex_error_info, Some(CodexErrorInfo::CyberPolicy));

    mock.single_request();

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyber_policy_response_retries_with_safe_guidance_on_each_turn_when_enabled() -> Result<()>
{
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let first_recovery_response = sse_response(sse(vec![
        ev_response_created("resp-recovery"),
        ev_assistant_message("msg-recovery", SAFE_RECOVERY_RESPONSE),
        core_test_support::responses::ev_completed("resp-recovery"),
    ]));
    let second_recovery_response = sse_response(sse(vec![
        ev_response_created("resp-second-recovery"),
        ev_assistant_message("msg-second-recovery", SECOND_SAFE_RECOVERY_RESPONSE),
        core_test_support::responses::ev_completed("resp-second-recovery"),
    ]));
    let mock = mount_response_sequence(
        &server,
        vec![
            cyber_policy_error_response(),
            first_recovery_response,
            cyber_policy_error_response(),
            second_recovery_response,
        ],
    )
    .await;

    let mut builder = test_codex()
        .with_model(REQUESTED_MODEL)
        .with_config(|config| {
            let _ = config.features.enable(Feature::CyberPolicyAutoRecovery);
        });
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, FIRST_RECOVERY_PROMPT))
        .await?;
    let first_outcome = collect_turn_outcome(&test).await;
    assert_eq!(
        first_outcome,
        (
            Vec::<String>::new(),
            vec![SAFE_RECOVERY_RESPONSE.to_string()]
        )
    );

    test.codex
        .submit(disabled_text_turn(&test, SECOND_RECOVERY_PROMPT))
        .await?;
    let second_outcome = collect_turn_outcome(&test).await;
    assert_eq!(
        second_outcome,
        (
            Vec::<String>::new(),
            vec![SECOND_SAFE_RECOVERY_RESPONSE.to_string()]
        )
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .map(|request| {
                request
                    .message_input_texts("developer")
                    .into_iter()
                    .filter(|text| text.starts_with("<cyber_policy_auto_recovery>"))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            Vec::<String>::new(),
            vec![CYBER_POLICY_AUTO_RECOVERY_FRAGMENT.to_string()],
            vec![CYBER_POLICY_AUTO_RECOVERY_FRAGMENT.to_string()],
            vec![
                CYBER_POLICY_AUTO_RECOVERY_FRAGMENT.to_string(),
                CYBER_POLICY_AUTO_RECOVERY_FRAGMENT.to_string(),
            ],
        ]
    );
    assert_eq!(
        [(1, FIRST_RECOVERY_PROMPT), (3, SECOND_RECOVERY_PROMPT)]
            .into_iter()
            .map(|(request_index, expected_prompt)| {
                requests[request_index]
                    .message_input_texts("user")
                    .into_iter()
                    .filter(|text| text == expected_prompt)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            vec![FIRST_RECOVERY_PROMPT.to_string()],
            vec![SECOND_RECOVERY_PROMPT.to_string()],
        ]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cyber_policy_auto_recovery_stops_after_repeated_block() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let mock = mount_response_sequence(
        &server,
        vec![cyber_policy_error_response(), cyber_policy_error_response()],
    )
    .await;

    let mut builder = test_codex()
        .with_model(REQUESTED_MODEL)
        .with_config(|config| {
            let _ = config.features.enable(Feature::CyberPolicyAutoRecovery);
        });
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(
            &test,
            "trigger repeated cyber policy error",
        ))
        .await?;

    let error = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = error else {
        panic!("expected error event");
    };
    assert_eq!(error.message, CYBER_POLICY_MESSAGE);
    assert_eq!(error.codex_error_info, Some(CodexErrorInfo::CyberPolicy));
    let _ = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]
            .message_input_texts("developer")
            .into_iter()
            .filter(|text| text.starts_with("<cyber_policy_auto_recovery>"))
            .collect::<Vec<_>>(),
        vec![CYBER_POLICY_AUTO_RECOVERY_FRAGMENT.to_string()]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_model_field_mismatch_emits_warning_when_header_matches_requested() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = sse_response(sse(vec![
        serde_json::json!({
            "type": "response.created",
            "response": {
                "id": "resp-1",
                "headers": {
                    "OpenAI-Model": SERVER_MODEL
                }
            }
        }),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", REQUESTED_MODEL);
    let _mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger response model check"))
        .await?;

    let reroute = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ModelReroute(_))
    })
    .await;
    let EventMsg::ModelReroute(reroute) = reroute else {
        panic!("expected model reroute event");
    };
    assert_eq!(reroute.from_model, REQUESTED_MODEL);
    assert_eq!(reroute.to_model, SERVER_MODEL);
    assert_eq!(reroute.reason, ModelRerouteReason::HighRiskCyberActivity);

    let warning = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::Warning(warning)
                if warning
                    .message
                    .contains("flagged for potentially high-risk cyber activity")
        )
    })
    .await;
    let EventMsg::Warning(warning) = warning else {
        panic!("expected warning event");
    };
    assert!(warning.message.contains(REQUESTED_MODEL));
    assert!(warning.message.contains(SERVER_MODEL));

    let _ = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_mismatch_only_emits_one_warning_per_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let tool_args = serde_json::json!({
        "command": "echo hello",
        "timeout_ms": 1_000
    });

    let first_response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            "call-1",
            "shell_command",
            &serde_json::to_string(&tool_args)?,
        ),
        core_test_support::responses::ev_completed("resp-1"),
    ]))
    .insert_header("OpenAI-Model", SERVER_MODEL);
    let second_response = sse_response(sse(vec![
        ev_response_created("resp-2"),
        ev_assistant_message("msg-1", "done"),
        core_test_support::responses::ev_completed("resp-2"),
    ]))
    .insert_header("OpenAI-Model", SERVER_MODEL);
    let _mock = mount_response_sequence(&server, vec![first_response, second_response]).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger follow-up turn"))
        .await?;

    let mut warning_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::Warning(warning)
                if warning
                    .message
                    .contains("flagged for potentially high-risk cyber activity") =>
            {
                warning_count += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(warning_count, 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_model_header_casing_only_mismatch_does_not_warn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let requested_header = REQUESTED_MODEL.to_ascii_uppercase();
    let response = sse_response(sse_completed("resp-1"))
        .insert_header("OpenAI-Model", requested_header.as_str());
    let _mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(REQUESTED_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger casing check"))
        .await?;

    let mut reroute_count = 0;
    let mut warning_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ModelReroute(_) => reroute_count += 1,
            EventMsg::Warning(warning)
                if warning
                    .message
                    .contains("flagged for potentially high-risk cyber activity") =>
            {
                warning_count += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(reroute_count, 0);
    assert_eq!(warning_count, 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_verification_emits_structured_event_without_reroute_or_warning() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_model_verification_metadata("resp-1", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        core_test_support::responses::ev_completed("resp-1"),
    ]));
    let _mock = mount_response_once(&server, response).await;

    let mut builder = test_codex().with_model(SERVER_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(&test, "trigger model verification"))
        .await?;

    let mut verification_count = 0;
    let mut reroute_count = 0;
    let mut warning_count = 0;
    let mut warning_item_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ModelVerification(event) => {
                assert_eq!(
                    event.verifications,
                    vec![ModelVerification::TrustedAccessForCyber]
                );
                verification_count += 1;
            }
            EventMsg::Warning(_) => warning_count += 1,
            EventMsg::ModelReroute(_) => reroute_count += 1,
            EventMsg::RawResponseItem(raw)
                if matches!(
                    &raw.item,
                    ResponseItem::Message { content, .. }
                        if content.iter().any(|item| matches!(
                            item,
                            ContentItem::InputText { text } if text.starts_with("Warning: ")
                        ))
                ) =>
            {
                warning_item_count += 1;
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(verification_count, 1);
    assert_eq!(reroute_count, 0);
    assert_eq!(warning_count, 0);
    assert_eq!(warning_item_count, 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_verification_only_emits_once_per_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let tool_args = serde_json::json!({
        "command": "echo hello",
        "timeout_ms": 1_000
    });

    let first_response = sse_response(sse(vec![
        ev_response_created("resp-1"),
        ev_function_call(
            "call-1",
            "shell_command",
            &serde_json::to_string(&tool_args)?,
        ),
        ev_model_verification_metadata("resp-1", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        core_test_support::responses::ev_completed("resp-1"),
    ]));
    let second_response = sse_response(sse(vec![
        ev_response_created("resp-2"),
        ev_model_verification_metadata("resp-2", vec![TRUSTED_ACCESS_FOR_CYBER_VERIFICATION]),
        ev_assistant_message("msg-1", "done"),
        core_test_support::responses::ev_completed("resp-2"),
    ]));
    let _mock = mount_response_sequence(&server, vec![first_response, second_response]).await;

    let mut builder = test_codex().with_model(SERVER_MODEL);
    let test = builder.build(&server).await?;

    test.codex
        .submit(disabled_text_turn(
            &test,
            "trigger follow-up model verification",
        ))
        .await?;

    let mut verification_count = 0;
    loop {
        let event = wait_for_event(&test.codex, |_| true).await;
        match event {
            EventMsg::ModelVerification(_) => verification_count += 1,
            EventMsg::Warning(warning) if warning.message.contains("high-risk cyber activity") => {
                panic!("model verification should not emit a warning event");
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    assert_eq!(verification_count, 1);

    Ok(())
}
