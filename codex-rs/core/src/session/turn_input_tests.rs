use super::*;
use crate::config::Constrained;
use crate::session::SessionSettingsUpdate;
use crate::session::step_settings::StepSettingsUpdate;
use crate::session::tests::make_session_and_context;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;
use codex_protocol::AgentPath;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::turn_input::AppServerClientInfo;
use codex_protocol::turn_input::TurnInput as SubmittedTurnInput;
use codex_protocol::user_input::UserInput;
use core_test_support::test_codex::local_selections;
use pretty_assertions::assert_eq;
use test_case::test_case;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
struct NeverEndingTask {
    kind: TaskKind,
    listen_to_cancellation_token: bool,
}

#[derive(Clone, Copy)]
enum CompetingStart {
    Shell,
    Workflow,
}

impl SessionTask for NeverEndingTask {
    fn kind(&self) -> TaskKind {
        self.kind
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn_input_test"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        if self.listen_to_cancellation_token {
            cancellation_token.cancelled().await;
            return Ok(None);
        }
        loop {
            sleep(std::time::Duration::from_secs(60)).await;
        }
    }
}

fn user_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

async fn submit_start_only(
    session: &Arc<Session>,
    input: SubmittedTurnInput,
) -> TurnInputSubmission {
    handle(
        session,
        TurnInputRequest::new(input),
        TurnInputMode::StartIfIdle,
        "test-submission".to_string(),
    )
    .await
    .expect("start-only submission should be valid")
}

async fn submit_steer_only(
    session: &Arc<Session>,
    input: Vec<UserInput>,
    expected_turn_id: &str,
) -> TurnInputSubmission {
    handle(
        session,
        TurnInputRequest::new(SubmittedTurnInput::UserInput {
            content: input,
            client_id: None,
        }),
        TurnInputMode::Steer {
            expected_turn_id: expected_turn_id.to_string(),
        },
        "test-submission".to_string(),
    )
    .await
    .expect("steer-only submission should be valid")
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "simulate an in-flight realtime append while checking input admission"
)]
async fn steering_does_not_wait_for_realtime_history() {
    let (mut session, turn_context) = make_session_and_context().await;
    session.realtime_history = Some(tokio::sync::Mutex::new(Default::default()));
    let session = Arc::new(session);
    let turn_context = Arc::new(turn_context);
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let history = session
        .realtime_history
        .as_ref()
        .expect("realtime history")
        .lock()
        .await;
    for mode in [
        TurnInputMode::StartOrSteer,
        TurnInputMode::Steer {
            expected_turn_id: turn_context.sub_id.clone(),
        },
    ] {
        let submission = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle(
                &session,
                TurnInputRequest::user_input(vec![UserInput::Text {
                    text: "steer without waiting for persistence".to_string(),
                    text_elements: Vec::new(),
                }]),
                mode,
                "steer-submission".to_string(),
            ),
        )
        .await
        .expect("steering must not wait for the realtime recorder")
        .expect("steering should succeed");
        assert_eq!(
            submission,
            TurnInputSubmission::Steered {
                turn_id: turn_context.sub_id.clone()
            }
        );
    }
    drop(history);
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn accepted_input_applies_thread_settings() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let config = session.get_config().await;
    handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }])
        .with_thread_settings(ThreadSettingsOverrides {
            environments: Some(local_selections(config.cwd.clone())),
            approval_policy: Some(config.permissions.approval_policy.value()),
            approvals_reviewer: Some(codex_config::types::ApprovalsReviewer::AutoReview),
            sandbox_policy: Some(config.legacy_sandbox_policy()),
            summary: config.model_reasoning_summary,
            personality: config.personality,
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: turn_context.model_info().slug.clone(),
                    reasoning_effort: config.model_reasoning_effort.clone(),
                    developer_instructions: None,
                },
            }),
            ..Default::default()
        }),
        TurnInputMode::StartOrSteer,
        "sub-1".to_string(),
    )
    .await
    .expect("submit user turn");

    let state = session.state.lock().await;
    assert_eq!(
        state.session_configuration.step_settings.approvals_reviewer,
        codex_config::types::ApprovalsReviewer::AutoReview
    );
    assert!(
        session.mcp_refresh.is_pending(),
        "server elicitation authority changes must refresh MCP state"
    );
}

#[tokio::test]
async fn start_only_rejects_active_turn_without_injecting() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let input = SubmittedTurnInput::ResponseItem(user_message("synthetic idle input"));
    let submission = submit_start_only(&session, input).await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        },
        submission
    );
    assert_eq!(
        Vec::<TurnInput>::new(),
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn rejected_user_idle_start_does_not_apply_app_server_client_info() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let submission = handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "must not mutate active turn".to_string(),
            text_elements: Vec::new(),
        }])
        .with_app_server_client_info(AppServerClientInfo {
            name: Some("rejected-client".to_string()),
            version: Some("9.9.9".to_string()),
            mcp_elicitations_auto_deny: true,
        }),
        TurnInputMode::StartUserIfIdle,
        "rejected-client-info".to_string(),
    )
    .await
    .expect("busy idle-only start should return typed rejection");
    assert_eq!(
        submission,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        }
    );
    let client_state = {
        let state = session.state.lock().await;
        (
            state.session_configuration.app_server_client_name.clone(),
            state
                .session_configuration
                .app_server_client_version
                .clone(),
            session.services.mcp_runtime.elicitations_auto_deny(),
        )
    };
    assert_eq!(client_state, (None, None, false));

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn recovery_rejects_active_turn_without_injecting_or_applying_settings() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    let original_approval_policy = session
        .get_config()
        .await
        .permissions
        .approval_policy
        .value();
    session
        .spawn_task(
            Arc::clone(&turn_context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;

    let submission = handle_recovery(
        &session,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
        TurnStartOptions::default(),
        "recovered-turn".to_string(),
    )
    .await
    .expect("recovery should return a typed rejection");

    assert_eq!(
        submission,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle,
        }
    );
    assert_eq!(
        session
            .get_config()
            .await
            .permissions
            .approval_policy
            .value(),
        original_approval_policy
    );
    assert_eq!(
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0,
        Vec::<TurnInput>::new()
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn start_only_rejects_current_plan_before_validating_settings() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let default_mode = session.collaboration_mode().await;
    {
        let mut state = session.state.lock().await;
        let settings = Arc::make_mut(&mut state.session_configuration.step_settings);
        settings.collaboration_mode.mode = ModeKind::Plan;
        settings.approval_policy = Constrained::allow_only(AskForApproval::OnRequest);
    }
    let desired_settings = session.thread_settings_snapshot().await;
    let invalid_override = ThreadSettingsOverrides {
        collaboration_mode: Some(default_mode.clone()),
        approval_policy: Some(AskForApproval::Never),
        ..Default::default()
    };

    // Current Plan takes precedence even when the request would leave Plan or
    // fail settings validation. Nothing has been reserved or applied yet.
    let submission = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(user_message(
            "synthetic idle input",
        )))
        .with_thread_settings(invalid_override.clone()),
        TurnInputMode::StartIfIdle,
        "automatic-plan-submission".to_string(),
    )
    .await
    .expect("current Plan must reject before settings validation");
    assert_eq!(
        submission,
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        }
    );
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert!(session.active_turn.lock().await.is_none());

    session
        .update_settings(SessionSettingsUpdate {
            step_settings: StepSettingsUpdate {
                collaboration_mode: Some(default_mode),
                ..Default::default()
            },
            ..Default::default()
        })
        .await
        .expect("explicit settings may leave Plan mode");
    let desired_settings = session.thread_settings_snapshot().await;
    let result = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(user_message(
            "invalid automatic input",
        )))
        .with_thread_settings(invalid_override),
        TurnInputMode::StartIfIdle,
        "invalid-automatic-submission".to_string(),
    )
    .await;
    let error = result.expect_err("invalid automatic settings must be rejected");
    assert!(matches!(
        error.details(),
        CodexErrorDetails::InvalidRequest(_)
    ));
    assert_eq!(session.thread_settings_snapshot().await, desired_settings);
    assert!(session.active_turn.lock().await.is_none());
    assert_eq!(
        Vec::<TurnInput>::new(),
        session
            .input_queue
            .get_pending_input(&session.active_turn)
            .await
            .0
    );
}

#[tokio::test]
async fn start_only_accepts_user_input_in_plan_mode() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let mut collaboration_mode = session.collaboration_mode().await;
    collaboration_mode.mode = ModeKind::Plan;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).collaboration_mode =
            collaboration_mode;
        state.merge_connector_selection(["calendar".to_string()]);
    }

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::UserInput {
            content: vec![UserInput::Text {
                text: "queued user input".to_string(),
                text_elements: Vec::new(),
            }],
            client_id: Some("queued-user-message".to_string()),
        },
    )
    .await;
    assert!(matches!(submission, TurnInputSubmission::Started { .. }));
    assert!(
        session
            .state
            .lock()
            .await
            .get_connector_selection()
            .is_empty()
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn start_or_steer_clears_connector_selection_when_starting() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    session
        .state
        .lock()
        .await
        .merge_connector_selection(["calendar".to_string()]);

    let submission = handle(
        &session,
        TurnInputRequest::user_input(vec![UserInput::Text {
            text: "new user turn".to_string(),
            text_elements: Vec::new(),
        }]),
        TurnInputMode::StartOrSteer,
        "new-user-turn".to_string(),
    )
    .await
    .expect("user turn should start");
    assert!(matches!(submission, TurnInputSubmission::Started { .. }));
    assert!(
        session
            .state
            .lock()
            .await
            .get_connector_selection()
            .is_empty()
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn start_only_rejects_empty_user_input_in_plan_mode() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let mut collaboration_mode = session.collaboration_mode().await;
    collaboration_mode.mode = ModeKind::Plan;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings).collaboration_mode =
            collaboration_mode;
    }

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::UserInput {
            content: Vec::new(),
            client_id: Some("empty-queued-user-message".to_string()),
        },
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        },
        submission
    );
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn start_only_rejects_pending_trigger_turn_without_injecting() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    {
        let mut state = session.state.lock().await;
        Arc::make_mut(&mut state.session_configuration.step_settings)
            .collaboration_mode
            .mode = ModeKind::Plan;
    }
    session
        .input_queue
        .enqueue_mailbox_communication(
            InterAgentCommunication::new(
                AgentPath::root(),
                AgentPath::root(),
                Vec::new(),
                "pending trigger".to_string(),
                /*trigger_turn*/ true,
            ),
            Default::default(),
        )
        .await;

    let submission = submit_start_only(
        &session,
        SubmittedTurnInput::ResponseItem(user_message("synthetic idle input")),
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        },
        submission
    );
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.input_queue.has_trigger_turn_mailbox_items().await);
    assert_eq!(session.collaboration_mode().await.mode, ModeKind::Plan);
}

#[tokio::test]
async fn steer_only_requires_active_turn() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let submission = submit_steer_only(
        &session,
        vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }],
        "missing-turn-id",
    )
    .await;

    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::NoActiveTurn,
        },
        submission
    );
}

#[tokio::test]
async fn realtime_input_cannot_replace_idle_turn_reservation() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let reserved_turn_state = idle_turn::reserve(&session)
        .await
        .expect("idle turn should be reserved");

    timeout(
        std::time::Duration::from_secs(5),
        session.route_realtime_text_input("realtime input during reservation".to_string()),
    )
    .await
    .expect("realtime routing should reject reservation promptly")
    .expect("realtime routing should not be draining");

    let reservation_state = {
        let active_turn = session.active_turn.lock().await;
        let active_turn = active_turn
            .as_ref()
            .expect("realtime input must preserve reservation");
        (
            active_turn.task.is_none(),
            Arc::ptr_eq(&active_turn.turn_state, &reserved_turn_state),
        )
    };
    assert_eq!(reservation_state, (true, true));
    let event = timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("realtime rejection event should arrive promptly")
        .expect("realtime rejection event");
    let EventMsg::Error(error) = event.msg else {
        panic!("expected realtime rejection error, got: {event:?}");
    };
    assert_eq!(error.message, "failed to submit turn input: NotIdle");
    assert_eq!(error.codex_error_info, Some(CodexErrorInfo::BadRequest));

    idle_turn::clear(&session, &reserved_turn_state).await;
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn shell_command_cannot_replace_idle_turn_reservation() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let reserved_turn_state = idle_turn::reserve(&session)
        .await
        .expect("idle turn should be reserved");

    crate::session::handlers::run_user_shell_command(
        &session,
        "shell-turn".to_string(),
        "echo must-not-run".to_string(),
        /*timeout_ms*/ Some(1_000),
    )
    .await;

    let reservation_state = {
        let active_turn = session.active_turn.lock().await;
        let active_turn = active_turn
            .as_ref()
            .expect("shell command must preserve reservation");
        (
            active_turn.task.is_none(),
            Arc::ptr_eq(&active_turn.turn_state, &reserved_turn_state),
        )
    };
    assert_eq!(reservation_state, (true, true));
    let event = timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("shell rejection event should arrive promptly")
        .expect("shell rejection event");
    let EventMsg::Error(error) = event.msg else {
        panic!("expected shell rejection error, got: {event:?}");
    };
    assert_eq!(
        error,
        ErrorEvent {
            misalignment: None,
            message: "Cannot run a shell command while a turn is starting.".to_string(),
            codex_error_info: Some(CodexErrorInfo::BadRequest),
        }
    );

    idle_turn::clear(&session, &reserved_turn_state).await;
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "hold active-turn lock to order both competing mutex waiters"
)]
async fn realtime_start_cannot_replace_reservation_acquired_after_idle_observation() {
    let (session, _turn_context, rx) = make_session_and_context_with_rx().await;
    let active_turn_guard = session.active_turn.lock().await;
    let realtime = session.route_realtime_text_input("stale realtime start".to_string());
    tokio::pin!(realtime);
    assert!(matches!(
        futures::poll!(&mut realtime),
        std::task::Poll::Pending
    ));
    let reservation = idle_turn::reserve(&session);
    tokio::pin!(reservation);
    assert!(matches!(
        futures::poll!(&mut reservation),
        std::task::Poll::Pending
    ));
    // Tokio mutex waiters are FIFO: realtime observes idle first, then the
    // competing reservation wins before realtime can reacquire for its start.
    drop(active_turn_guard);
    let (realtime_result, reservation_result) = timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(realtime, reservation)
    })
    .await
    .expect("both ordered contenders should resolve promptly");
    realtime_result.expect("realtime routing should not be draining");
    let reserved_turn_state =
        reservation_result.expect("idle turn should be reserved after realtime observed idle");
    let reservation_state = {
        let active_turn = session.active_turn.lock().await;
        let active_turn = active_turn
            .as_ref()
            .expect("stale realtime start must preserve newer reservation");
        (
            active_turn.task.is_none(),
            Arc::ptr_eq(&active_turn.turn_state, &reserved_turn_state),
        )
    };
    assert_eq!(reservation_state, (true, true));
    let event = timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("stale realtime rejection should arrive promptly")
        .expect("stale realtime rejection event");
    let EventMsg::Error(error) = event.msg else {
        panic!("expected stale realtime rejection error, got: {event:?}");
    };
    assert_eq!(error.message, "failed to submit turn input: NotIdle");

    idle_turn::clear(&session, &reserved_turn_state).await;
    assert!(session.active_turn.lock().await.is_none());
}

#[test_case(CompetingStart::Shell; "shell_command")]
#[test_case(CompetingStart::Workflow; "workflow_command")]
#[tokio::test]
async fn realtime_start_is_serialized_with_other_start_paths(competing_start: CompetingStart) {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let turn_start_guard = session.acquire_turn_start_lock().await;
    let realtime = session.route_realtime_text_input("serialized realtime start".to_string());
    tokio::pin!(realtime);
    assert!(matches!(
        futures::poll!(&mut realtime),
        std::task::Poll::Pending
    ));
    let competing_start = async {
        match competing_start {
            CompetingStart::Shell => {
                crate::session::handlers::run_user_shell_command(
                    &session,
                    "competing-turn".to_string(),
                    "echo serialized".to_string(),
                    /*timeout_ms*/ Some(1_000),
                )
                .await;
            }
            CompetingStart::Workflow => {
                crate::session::handlers::run_workflow_command(
                    &session,
                    "competing-turn".to_string(),
                    std::path::PathBuf::from("unused"),
                    serde_json::Value::Null,
                )
                .await;
            }
        }
    };
    tokio::pin!(competing_start);
    assert!(matches!(
        futures::poll!(&mut competing_start),
        std::task::Poll::Pending
    ));
    drop(turn_start_guard);

    timeout(std::time::Duration::from_secs(5), async {
        let (realtime_result, ()) = tokio::join!(realtime, competing_start);
        realtime_result.expect("realtime routing should not be draining");
    })
    .await
    .expect("serialized starts should resolve promptly");

    let active_turn_id = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .and_then(|active_turn| active_turn.task.as_ref())
        .map(|task| task.turn_context.sub_id.clone());
    assert!(active_turn_id.is_some_and(|turn_id| turn_id != "competing-turn"));

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn steer_only_enforces_expected_turn_id() {
    let (session, turn_context, _rx) = make_session_and_context_with_rx().await;
    turn_context
        .turn_metadata_state
        .set_turn_trigger("composer".to_string());
    session
        .spawn_task(
            Arc::clone(&turn_context),
            vec![TurnInput::UserInput {
                acceptance_order: None,
                content: vec![UserInput::Text {
                    text: "hello".to_string(),
                    text_elements: Vec::new(),
                }],
                client_id: None,
            }],
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: false,
            },
        )
        .await;

    let submission = submit_steer_only(
        &session,
        vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }],
        "different-turn-id",
    )
    .await;
    assert_eq!(
        TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ExpectedTurnMismatch {
                expected: "different-turn-id".to_string(),
                actual: turn_context.sub_id.clone(),
            },
        },
        submission
    );

    let output: ResponseItem = serde_json::from_value(serde_json::json!({
        "type": "function_call_output",
        "name": "send_message_to_thread",
        "output": "delegated work",
    }))
    .expect("valid standalone output");

    let submission = handle(
        &session,
        TurnInputRequest::new(SubmittedTurnInput::ResponseItem(output)).on_start(
            TurnStartOptions {
                turn_trigger: Some("automation_cron_scheduled".to_string()),
                ..Default::default()
            },
        ),
        TurnInputMode::StartOrSteer,
        "test-submission".to_string(),
    )
    .await
    .expect("standalone output should steer the active turn");

    assert_eq!(
        submission,
        TurnInputSubmission::Steered {
            turn_id: turn_context.sub_id.clone()
        }
    );
    assert_eq!(
        turn_context
            .turn_metadata_state
            .current_turn_trigger()
            .as_deref(),
        Some("composer")
    );
    let turn_state = session
        .input_queue
        .turn_state_for_sub_id(&session.active_turn, &turn_context.sub_id)
        .await
        .expect("active turn state");
    assert_eq!(
        session
            .input_queue
            .subscribe_activity(Some(turn_state.as_ref()))
            .await
            .1,
        Some(crate::session::input_queue::InputQueueActivity::Steer)
    );
}

#[tokio::test]
async fn rejects_non_regular_turns() {
    for (task_kind, turn_kind) in [(TaskKind::Compact, NonSteerableTurnKind::Compact)] {
        let (session, incoming_turn_context, _rx) = make_session_and_context_with_rx().await;
        incoming_turn_context
            .turn_metadata_state
            .set_root_turn_id("incoming-root".to_string());
        let turn_context = session
            .new_turn_with_default_settings("turn".to_string(), Default::default())
            .await;
        turn_context
            .turn_metadata_state
            .set_root_turn_id("active-root".to_string());
        session
            .spawn_task(
                Arc::clone(&turn_context),
                vec![TurnInput::UserInput {
                    acceptance_order: None,
                    content: vec![UserInput::Text {
                        text: "hello".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
                NeverEndingTask {
                    kind: task_kind,
                    listen_to_cancellation_token: true,
                },
            )
            .await;

        let steer_input = vec![UserInput::Text {
            text: "steer".to_string(),
            text_elements: Vec::new(),
        }];
        let steer_submission = submit_steer_only(&session, steer_input.clone(), "turn").await;
        assert_eq!(
            TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ActiveTurnNotSteerable { turn_kind },
            },
            steer_submission
        );
        let start_or_steer_submission = handle(
            &session,
            TurnInputRequest::user_input(steer_input),
            TurnInputMode::StartOrSteer,
            "test-submission".to_string(),
        )
        .await
        .expect("start-or-steer submission should be valid");
        assert_eq!(
            TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ActiveTurnNotSteerable { turn_kind },
            },
            start_or_steer_submission
        );
        assert_eq!(
            turn_context.turn_metadata_state.root_turn_id().as_deref(),
            Some("active-root")
        );

        session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    }
}
