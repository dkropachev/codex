use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use crate::app_server_session::AppServerStartedThread;
use crate::handoff::HandoffDisposition;
use crate::handoff::HandoffTrigger;
use crate::handoff::PendingHandoffPlan;
use codex_features::Feature;
use codex_protocol::protocol::AskForApproval as CoreAskForApproval;
use pretty_assertions::assert_eq;

const HANDOFF_PLAN: &str = "- Preserve completed work\n- Finish the focused validation";

fn arm_handoff_transfer(app: &mut App, disposition: HandoffDisposition) -> u64 {
    let mask = crate::handoff::handoff_mask(app.model_catalog.as_ref())
        .expect("handoff collaboration mode");
    app.chat_widget.set_collaboration_mask(mask);
    app.chat_widget
        .resume_handoff_after_transfer_failure(HandoffTrigger::Manual, disposition);
    app.chat_widget
        .active_handoff_generation()
        .expect("active handoff generation")
}

fn configure_isolated_app(app: &mut App, temp: &tempfile::TempDir) -> Result<PathBuf> {
    let codex_home = temp.path().join("home");
    let cwd = temp.path().join("project");
    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&cwd)?;
    std::fs::write(
        codex_home.join("config.toml"),
        "model_reasoning_effort = \"low\"\n[features]\nfast_mode = true\n",
    )?;

    app.config.codex_home = codex_home.abs();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.abs());
    app.config.cwd = cwd.abs();
    app.config.model = Some("gpt-5.2".to_string());
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);
    app.config.features.enable(Feature::FastMode)?;
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    app.config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            PermissionProfile::workspace_write(),
        ))?;
    app.harness_overrides.model = Some("gpt-5.2".to_string());
    app.harness_overrides.approval_policy = Some(CoreAskForApproval::OnRequest);
    app.harness_overrides.permission_profile = Some(PermissionProfile::workspace_write());
    Ok(cwd)
}

async fn attach_recording_source(
    app: &mut App,
    app_server: &mut AppServerSession,
) -> Result<ThreadId> {
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app_server
        .thread_inject_items(thread_id, vec![App::side_boundary_prompt_item()])
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    Ok(thread_id)
}

fn submitted_user_turns(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    active_thread_id: ThreadId,
) -> Vec<(ThreadId, AppCommand)> {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::CodexOp(op) if matches!(op, AppCommand::UserTurn { .. }) => {
                Some((active_thread_id, op))
            }
            AppEvent::SubmitThreadOp { thread_id, op }
                if matches!(op, AppCommand::UserTurn { .. }) =>
            {
                Some((thread_id, op))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn plan_handoff_fresh_start_failure_keeps_source_thread_and_ui_intact() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let temp = tempfile::tempdir()?;
    configure_isolated_app(&mut app, &temp)?;
    let (mut app_server, requests, proxy) = session_lifecycle_requests::start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let source_thread_id = attach_recording_source(&mut app, &mut app_server).await?;
    app.transcript_cells = vec![plain_line_cell("source transcript remains visible")];
    let source_active_thread_id = app.active_thread_id;
    let source_primary_thread_id = app.primary_thread_id;

    proxy.abort();
    assert!(
        proxy
            .await
            .expect_err("proxy task should be cancelled")
            .is_cancelled()
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let generation = arm_handoff_transfer(&mut app, HandoffDisposition::Proceed);
    app.start_handoff_transfer(
        &mut tui,
        &mut app_server,
        source_thread_id,
        generation,
        HANDOFF_PLAN.to_string(),
        HandoffDisposition::Proceed,
        HandoffTrigger::Manual,
    )
    .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, source_active_thread_id);
    assert_eq!(app.primary_thread_id, source_primary_thread_id);
    assert!(app.chat_widget.composer_text_with_pending().is_empty());
    assert_eq!(app.transcript_cells.len(), 1);
    assert!(app.overlay.is_none());
    let methods = requests
        .lock()
        .expect("request recorder lock")
        .iter()
        .map(|request| request.method.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "thread/start")
            .count(),
        1
    );
    assert!(!methods.iter().any(|method| method == "thread/unsubscribe"));
    Ok(())
}

#[tokio::test]
async fn plan_handoff_final_transfer_gate_rechecks_active_descendants() -> Result<()> {
    let (mut app, _events, _ops) = make_test_app_with_channels().await;
    let temp = tempfile::tempdir()?;
    configure_isolated_app(&mut app, &temp)?;
    let (mut app_server, requests, proxy) = session_lifecycle_requests::start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let source_thread_id = attach_recording_source(&mut app, &mut app_server).await?;
    let child = ThreadId::new();
    app.agent_navigation.upsert(
        child, /*agent_nickname*/ None, /*agent_role*/ None, /*is_closed*/ false,
    );
    app.agent_navigation.mark_running(child);
    let generation = arm_handoff_transfer(&mut app, HandoffDisposition::Proceed);
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.start_handoff_transfer(
        &mut tui,
        &mut app_server,
        source_thread_id,
        generation,
        HANDOFF_PLAN.to_string(),
        HandoffDisposition::Proceed,
        HandoffTrigger::Manual,
    )
    .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(
        session_lifecycle_requests::recorded_params(&requests, "thread/start").len(),
        1,
        "the final safety gate must run before a fresh destination starts"
    );
    app.agent_navigation.mark_closed(child);
    app.start_handoff_transfer(
        &mut tui,
        &mut app_server,
        source_thread_id,
        generation,
        HANDOFF_PLAN.to_string(),
        HandoffDisposition::Proceed,
        HandoffTrigger::Manual,
    )
    .await?;
    assert_eq!(
        session_lifecycle_requests::recorded_params(&requests, "thread/start").len(),
        1,
        "a stale pre-failure generation must not retry the transfer"
    );
    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn plan_handoff_defer_installs_runtime_plan_and_restores_it_on_direct_resume() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let temp = tempfile::tempdir()?;
    configure_isolated_app(&mut app, &temp)?;
    let (mut app_server, requests, proxy) = session_lifecycle_requests::start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let source_thread_id = attach_recording_source(&mut app, &mut app_server).await?;
    let source_session = app
        .primary_session_configured
        .clone()
        .expect("source session should be configured");
    while events.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let generation = arm_handoff_transfer(&mut app, HandoffDisposition::Defer);

    app.start_handoff_transfer(
        &mut tui,
        &mut app_server,
        source_thread_id,
        generation,
        HANDOFF_PLAN.to_string(),
        HandoffDisposition::Defer,
        HandoffTrigger::Manual,
    )
    .await?;

    let destination_thread_id = app
        .chat_widget
        .thread_id()
        .expect("deferred handoff destination thread");
    let destination_session = app
        .primary_session_configured
        .clone()
        .expect("destination session should be configured");
    assert_ne!(destination_thread_id, source_thread_id);
    let expected_pending =
        PendingHandoffPlan::new(HANDOFF_PLAN.to_string()).expect("valid handoff plan");
    assert_eq!(
        app.pending_handoffs
            .get(&destination_thread_id)
            .map(crate::handoff::PendingHandoffState::plan),
        Some(&expected_pending)
    );
    assert_eq!(
        app.chat_widget.pending_handoff_plan(),
        Some(&expected_pending)
    );
    assert!(
        submitted_user_turns(&mut events, destination_thread_id).is_empty(),
        "defer must not submit the plan to the model"
    );

    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: source_session.clone(),
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.chat_widget.pending_handoff_plan(), None);

    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: destination_session.clone(),
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(destination_thread_id));
    assert_eq!(
        app.chat_widget.pending_handoff_plan(),
        Some(&expected_pending)
    );
    assert!(submitted_user_turns(&mut events, destination_thread_id).is_empty());

    app.chat_widget
        .restore_user_message_to_composer("continue deferred work".into());
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::Enter));
    let submitted = submitted_user_turns(&mut events, destination_thread_id);
    assert_eq!(submitted.len(), 1);
    let (_, execution_op) = submitted.into_iter().next().expect("one deferred turn");
    app.handle_event(&mut tui, &mut app_server, AppEvent::CodexOp(execution_op))
        .await?;
    app.chat_widget.handle_server_notification(
        turn_started_notification(destination_thread_id, "deferred-execution"),
        /*replay_kind*/ None,
    );
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .all(|event| !matches!(event, AppEvent::PendingHandoffConsumed { .. })),
        "an unrelated turn/start must not consume the pending handoff"
    );
    let committed_prompt =
        expected_pending.execution_prompt_with_instruction("continue deferred work");
    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: destination_thread_id.to_string(),
            turn_id: "deferred-execution".to_string(),
            completed_at_ms: 0,
            item: codex_app_server_protocol::ThreadItem::UserMessage {
                id: "deferred-execution-user-message".to_string(),
                client_id: None,
                content: vec![codex_app_server_protocol::UserInput::Text {
                    text: committed_prompt,
                    text_elements: Vec::new(),
                }],
            },
        }),
        /*replay_kind*/ None,
    );
    let consumed = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::PendingHandoffConsumed { .. }))
        .expect("the committed prompt should acknowledge pending handoff consumption");
    app.handle_event(&mut tui, &mut app_server, consumed)
        .await?;
    assert_eq!(app.chat_widget.pending_handoff_plan(), None);
    assert!(!app.pending_handoffs.contains_key(&destination_thread_id));

    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: source_session,
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: destination_session,
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    assert_eq!(app.chat_widget.pending_handoff_plan(), None);
    assert_eq!(
        session_lifecycle_requests::recorded_params(&requests, "turn/start").len(),
        1,
        "the deferred plan must be submitted exactly once"
    );

    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[tokio::test]
async fn plan_handoff_proceed_preserves_settings_and_submits_first_fresh_turn() -> Result<()> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    let temp = tempfile::tempdir()?;
    let cwd = configure_isolated_app(&mut app, &temp)?;
    let (mut app_server, requests, proxy) = session_lifecycle_requests::start_recording_app_server(
        &app.config,
        /*blocked_thread_list*/ None,
        /*failed_thread_name*/ None,
    )
    .await?;
    let source_thread_id = attach_recording_source(&mut app, &mut app_server).await?;
    set_chatgpt_auth(&mut app.chat_widget);
    set_fast_mode_test_catalog(&mut app.chat_widget);
    app.model_catalog = app.chat_widget.model_catalog();
    app.chat_widget
        .set_feature_enabled(Feature::FastMode, /*enabled*/ true);
    app.chat_widget.set_model("gpt-5.4");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_service_tier(Some(ServiceTier::Fast.request_value().to_string()));
    let expected_permission_profile = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(&[cwd.clone().abs()]);
    while events.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let generation = arm_handoff_transfer(&mut app, HandoffDisposition::Proceed);

    app.start_handoff_transfer(
        &mut tui,
        &mut app_server,
        source_thread_id,
        generation,
        HANDOFF_PLAN.to_string(),
        HandoffDisposition::Proceed,
        HandoffTrigger::Manual,
    )
    .await?;

    let destination_thread_id = app
        .chat_widget
        .thread_id()
        .expect("proceed handoff destination thread");
    assert_ne!(destination_thread_id, source_thread_id);
    assert_eq!(app.chat_widget.current_model(), "gpt-5.4");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.chat_widget.current_service_tier(),
        Some(ServiceTier::Fast.request_value())
    );
    assert_eq!(app.chat_widget.config_ref().cwd.as_path(), cwd);
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .approval_policy
            .value(),
        AskForApproval::OnRequest.to_core()
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &expected_permission_profile
    );

    let expected_prompt = PendingHandoffPlan::new(HANDOFF_PLAN.to_string())
        .expect("valid handoff plan")
        .execution_prompt();
    assert_eq!(
        app.chat_widget
            .pending_handoff_state()
            .and_then(crate::handoff::PendingHandoffState::submitted_text),
        Some(expected_prompt.as_str())
    );
    assert!(app.pending_handoffs.contains_key(&destination_thread_id));
    let submitted = submitted_user_turns(&mut events, destination_thread_id);
    assert_eq!(submitted.len(), 1);
    let (
        submitted_thread_id,
        AppCommand::UserTurn {
            items,
            collaboration_mode,
            ..
        },
    ) = &submitted[0]
    else {
        unreachable!("submitted_user_turns only returns user turns");
    };
    assert_eq!(*submitted_thread_id, destination_thread_id);
    assert_eq!(
        items,
        &vec![UserInput::Text {
            text: expected_prompt.clone(),
            text_elements: Vec::new(),
        }]
    );
    assert_eq!(
        collaboration_mode.as_ref().map(|mode| mode.mode),
        Some(ModeKind::Default)
    );
    let (_, execution_op) = submitted
        .into_iter()
        .next()
        .expect("one submitted execution turn");
    app.handle_event(&mut tui, &mut app_server, AppEvent::CodexOp(execution_op))
        .await?;
    assert!(app.pending_handoffs.contains_key(&destination_thread_id));

    let starts = session_lifecycle_requests::recorded_params(&requests, "thread/start");
    assert_eq!(starts.len(), 2);
    let handoff_start = &starts[1];
    assert_eq!(handoff_start["model"], "gpt-5.4");
    assert_eq!(handoff_start["config"]["model_reasoning_effort"], "high");
    assert_eq!(
        handoff_start["serviceTier"],
        ServiceTier::Fast.request_value()
    );
    assert_eq!(handoff_start["cwd"], cwd.display().to_string());
    assert_eq!(handoff_start["approvalPolicy"], "on-request");
    assert_eq!(handoff_start["sandbox"], "workspace-write");
    assert_eq!(handoff_start["sessionStartSource"], "clear");
    let execution_turns = session_lifecycle_requests::recorded_params(&requests, "turn/start");
    assert_eq!(execution_turns.len(), 1);
    assert_eq!(
        execution_turns[0]["threadId"],
        destination_thread_id.to_string()
    );
    assert_eq!(execution_turns[0]["input"][0]["text"], expected_prompt);
    assert_eq!(execution_turns[0]["model"], "gpt-5.4");
    assert_eq!(execution_turns[0]["effort"], "high");
    assert_eq!(
        execution_turns[0]["serviceTier"],
        ServiceTier::Fast.request_value()
    );
    assert_eq!(execution_turns[0]["collaborationMode"]["mode"], "default");

    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: destination_thread_id.to_string(),
            turn_id: "handoff-execution".to_string(),
            completed_at_ms: 0,
            item: codex_app_server_protocol::ThreadItem::UserMessage {
                id: "handoff-execution-user".to_string(),
                client_id: None,
                content: vec![codex_app_server_protocol::UserInput::Text {
                    text: expected_prompt,
                    text_elements: Vec::new(),
                }],
            },
        }),
        /*replay_kind*/ None,
    );
    let consumed = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::PendingHandoffConsumed { .. }))
        .expect("exact committed execution prompt should complete proceed handoff");
    app.handle_event(&mut tui, &mut app_server, consumed)
        .await?;
    assert_eq!(app.chat_widget.pending_handoff_plan(), None);
    assert!(!app.pending_handoffs.contains_key(&destination_thread_id));

    let resumed_source = app_server
        .resume_thread(
            app.config.clone(),
            source_thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
        )
        .await?;
    assert_eq!(resumed_source.session.thread_id, source_thread_id);

    app_server.shutdown().await?;
    proxy.await??;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomaticGate {
    NonPrimary,
    Overlay,
    PendingUserInput,
    ParentOwned,
    ActiveDescendant,
    Eligible,
}

async fn latched_automatic_app() -> Result<(
    App,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ThreadId,
)> {
    let (mut app, mut events, _ops) = make_test_app_with_channels().await;
    app.config.tui_auto_handoff_threshold_percent = Some(71);
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let init = app.chatwidget_init_for_forked_or_resumed_thread(
        &mut tui,
        app.config.clone(),
        /*initial_user_message*/ None,
    );
    app.replace_chat_widget(ChatWidget::new_with_app_event(init));
    let thread_id = ThreadId::new();
    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "work-turn"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        ServerNotification::ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification {
            thread_id: thread_id.to_string(),
            turn_id: "work-turn".to_string(),
            token_usage: ThreadTokenUsage {
                total: TokenUsageBreakdown {
                    total_tokens: 400_000,
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_output_tokens: 0,
                },
                last: TokenUsageBreakdown {
                    total_tokens: 74_480,
                    input_tokens: 0,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 0,
                    reasoning_output_tokens: 0,
                },
                model_context_window: Some(100_000),
            },
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        turn_completed_notification(thread_id, "work-turn", TurnStatus::Completed),
        /*replay_kind*/ None,
    );
    assert!(app.chat_widget.automatic_handoff_is_locally_eligible());
    while events.try_recv().is_ok() {}
    Ok((app, events, thread_id))
}

#[tokio::test]
async fn plan_handoff_automatic_candidate_obeys_app_safety_gates() -> Result<()> {
    for gate in [
        AutomaticGate::NonPrimary,
        AutomaticGate::Overlay,
        AutomaticGate::PendingUserInput,
        AutomaticGate::ParentOwned,
        AutomaticGate::ActiveDescendant,
        AutomaticGate::Eligible,
    ] {
        let (mut app, mut events, thread_id) = latched_automatic_app().await?;
        match gate {
            AutomaticGate::NonPrimary => app.primary_thread_id = Some(ThreadId::new()),
            AutomaticGate::Overlay => {
                app.overlay = Some(Overlay::new_transcript(
                    Vec::new(),
                    crate::keymap::RuntimeKeymap::defaults().pager,
                ));
            }
            AutomaticGate::PendingUserInput => {
                assert_eq!(
                    app.pending_app_server_requests.note_server_request(
                        &request_user_input_request(thread_id, "work-turn", "question-1")
                    ),
                    None
                );
            }
            AutomaticGate::ParentOwned => app.agent_navigation.mark_parent_owned(thread_id),
            AutomaticGate::ActiveDescendant => {
                let child = ThreadId::new();
                app.agent_navigation.upsert(
                    child, /*agent_nickname*/ None, /*agent_role*/ None,
                    /*is_closed*/ false,
                );
                app.agent_navigation.mark_running(child);
            }
            AutomaticGate::Eligible => {}
        }

        app.maybe_start_automatic_handoff(thread_id).await;

        let submitted = submitted_user_turns(&mut events, thread_id);
        if gate == AutomaticGate::Eligible {
            assert_eq!(submitted.len(), 1);
            let (submitted_thread_id, AppCommand::UserTurn { items, .. }) = &submitted[0] else {
                unreachable!("submitted_user_turns only returns user turns");
            };
            assert_eq!(*submitted_thread_id, thread_id);
            assert_eq!(
                items,
                &vec![UserInput::Text {
                    text: crate::handoff::AUTOMATIC_WRAP_UP_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }]
            );
        } else {
            assert!(
                submitted.is_empty(),
                "{gate:?} must block automatic handoff"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn plan_handoff_automatic_planning_gate_rechecks_app_owned_blockers() -> Result<()> {
    let (mut app, mut events, thread_id) = latched_automatic_app().await?;
    app.maybe_start_automatic_handoff(thread_id).await;
    let submitted = submitted_user_turns(&mut events, thread_id);
    let (_, AppCommand::UserTurn { items, .. }) = submitted
        .into_iter()
        .next()
        .expect("automatic wrap-up submission")
    else {
        unreachable!("submitted_user_turns only returns user turns");
    };

    app.chat_widget
        .bind_handoff_turn_start("wrap-up-turn", &items);
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "wrap-up-turn"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: thread_id.to_string(),
            turn_id: "wrap-up-turn".to_string(),
            completed_at_ms: 0,
            item: codex_app_server_protocol::ThreadItem::UserMessage {
                id: "wrap-up-user".to_string(),
                client_id: None,
                content: items,
            },
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        turn_completed_notification(thread_id, "wrap-up-turn", TurnStatus::Completed),
        /*replay_kind*/ None,
    );
    let generation = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::AdvanceAutomaticHandoffPlanning { generation, .. } => Some(generation),
            _ => None,
        })
        .expect("wrap-up should queue the app planning gate");

    app.overlay = Some(Overlay::new_transcript(
        Vec::new(),
        crate::keymap::RuntimeKeymap::defaults().pager,
    ));
    app.maybe_advance_automatic_handoff_planning(thread_id, generation)
        .await;

    assert!(submitted_user_turns(&mut events, thread_id).is_empty());
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Default
    );
    Ok(())
}

#[tokio::test]
async fn plan_handoff_closed_descendant_reissues_the_latched_candidate() -> Result<()> {
    let (mut app, mut events, thread_id) = latched_automatic_app().await?;
    while events.try_recv().is_ok() {}
    let child = ThreadId::new();
    app.agent_navigation.upsert(
        child, /*agent_nickname*/ None, /*agent_role*/ None, /*is_closed*/ false,
    );
    app.agent_navigation.mark_running(child);
    app.thread_event_channels.insert(
        child,
        ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY),
    );
    app.maybe_start_automatic_handoff(thread_id).await;
    assert!(submitted_user_turns(&mut events, thread_id).is_empty());

    app.enqueue_thread_notification(
        child,
        ServerNotification::ThreadClosed(ThreadClosedNotification {
            thread_id: child.to_string(),
        }),
    )
    .await?;

    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::AutomaticHandoffCandidate { thread_id: candidate } if candidate == thread_id
        ))
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_invalid_type)]
async fn plan_handoff_descendant_lock_contention_waits_instead_of_cancelling() -> Result<()> {
    let (mut app, mut events, thread_id) = latched_automatic_app().await?;
    let child = ThreadId::new();
    app.agent_navigation.upsert(
        child, /*agent_nickname*/ None, /*agent_role*/ None, /*is_closed*/ false,
    );
    let channel = ThreadEventChannel::new(THREAD_EVENT_CHANNEL_CAPACITY);
    let store = Arc::clone(&channel.store);
    app.thread_event_channels.insert(child, channel);
    let guard = store.lock().await;
    let mut start = Box::pin(app.maybe_start_automatic_handoff(thread_id));

    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut start)
            .await
            .is_err()
    );
    drop(guard);
    start.await;

    assert_eq!(submitted_user_turns(&mut events, thread_id).len(), 1);
    Ok(())
}

#[tokio::test]
async fn plan_handoff_root_reattach_preserves_passive_hint_and_no_retry_state() -> Result<()> {
    let (mut app, _events, source_thread_id) = latched_automatic_app().await?;
    app.chat_widget.cancel_automatic_handoff_for_navigation();
    let expected = app.chat_widget.passive_handoff_state();
    assert!(expected.context_hint_shown);
    assert!(expected.automatic_cancelled_until_rearm);
    let source_session = app
        .primary_session_configured
        .clone()
        .expect("source session");
    let destination_thread_id = ThreadId::new();
    let destination_session =
        test_thread_session(destination_thread_id, app.config.cwd.to_path_buf());
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: destination_session,
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(destination_thread_id));

    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        AppServerStartedThread {
            session: source_session,
            turns: Vec::new(),
            blocks_direct_input: false,
            task_tools_available: false,
        },
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.chat_widget.passive_handoff_state(), expected);
    assert!(!app.chat_widget.automatic_handoff_is_locally_eligible());
    Ok(())
}
