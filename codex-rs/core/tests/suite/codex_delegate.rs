use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_wine_exec;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

#[cfg_attr(windows, ignore = "no exec_command on Windows")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guardian_delegate_rejects_escalation_requests_without_prompting() {
    skip_if_wine_exec!("Guardian approval actions require host-native paths");
    skip_if_no_network!();

    let server = start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
            config
                .permissions
                .set_permission_profile(PermissionProfile::read_only())
                .expect("guardian delegate should inherit a restricted permission profile");
        })
        .build_with_auto_env(&server)
        .await
        .expect("build guardian delegate with escalation support");

    let parent_call_id = "parent-escalation-call";
    let guardian_call_id = "guardian-escalation-call";
    let guardian_output_file = test.cwd.path().join("guardian-escalation-marker.txt");
    let parent_command = serde_json::json!({
        "cmd": "echo parent command",
        "sandbox_permissions": "require_escalated",
        "justification": "Trigger Guardian approval review."
    });
    let guardian_command = serde_json::json!({
        "cmd": format!("echo guardian-ran > \"{}\"", guardian_output_file.display()),
        "sandbox_permissions": "require_escalated",
        "justification": "Guardian must not escalate its own commands."
    });
    let assessment = serde_json::json!({
        "risk_level": "high",
        "user_authorization": "low",
        "outcome": "deny",
        "rationale": "Guardian could not execute an escalated command."
    });
    let response_mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-parent-command"),
                ev_function_call(parent_call_id, "exec_command", &parent_command.to_string()),
                ev_completed("resp-parent-command"),
            ]),
            sse(vec![
                ev_response_created("resp-guardian-command"),
                ev_function_call(
                    guardian_call_id,
                    "exec_command",
                    &guardian_command.to_string(),
                ),
                ev_completed("resp-guardian-command"),
            ]),
            sse(vec![
                ev_response_created("resp-guardian-assessment"),
                ev_assistant_message("msg-guardian-assessment", &assessment.to_string()),
                ev_completed("resp-guardian-assessment"),
            ]),
            sse(vec![
                ev_response_created("resp-parent-denied"),
                ev_assistant_message("msg-parent-denied", "denied"),
                ev_completed("resp-parent-denied"),
            ]),
        ],
    )
    .await;

    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Trigger Guardian review of an escalated command".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                approval_policy: Some(AskForApproval::OnRequest),
                approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
                ..Default::default()
            }),
        )
        .await
        .expect("submit guardian-reviewed command");

    let event = wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::ExecApprovalRequest(_) | EventMsg::TurnComplete(_)
        )
    })
    .await;
    assert!(
        matches!(event, EventMsg::TurnComplete(_)),
        "guardian delegate should reject escalation requests without prompting: {event:?}"
    );

    let requests = response_mock.requests();
    let guardian_requests = requests
        .iter()
        .filter(|request| {
            request.body_json()["client_metadata"]["x-openai-subagent"].as_str() == Some("guardian")
        })
        .collect::<Vec<_>>();
    assert_eq!(guardian_requests.len(), 2);
    let guardian_output = guardian_requests
        .iter()
        .find_map(|request| request.function_call_output_text(guardian_call_id))
        .expect("guardian continuation should include the rejected command output");
    assert!(
        guardian_output.contains("approval policy is Never"),
        "guardian escalation should be rejected by its never approval policy: {guardian_output}"
    );
    assert!(
        !guardian_output_file.exists(),
        "guardian command requiring approval should never execute"
    );

    let parent_output = requests
        .iter()
        .find_map(|request| request.function_call_output_text(parent_call_id))
        .expect("parent continuation should include the guardian denial");
    assert!(
        parent_output.contains("Guardian could not execute an escalated command."),
        "guardian denial rationale should reach the parent: {parent_output}"
    );
}
