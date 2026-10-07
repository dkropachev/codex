use pretty_assertions::assert_eq;

use super::*;
use crate::chatwidget::UserMessage;
use crate::slash_command::SlashCommand;
use codex_app_server_protocol::ItemCompletedNotification;

struct PreparedHandoffApp {
    app: Box<App>,
    events: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tui: crate::tui::Tui,
    app_server: AppServerSession,
    source_thread_id: ThreadId,
    transfer: AppEvent,
}

async fn prepare_handoff_on_server(
    app: &mut App,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ops: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
    app_server: &mut AppServerSession,
) -> Result<(ThreadId, AppEvent)> {
    let source_thread_id =
        prepare_handoff_plan_on_server(app, events, ops, app_server, "/handoff").await?;
    let transfer = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
        .expect("validated plan should request a fresh transfer");
    Ok((source_thread_id, transfer))
}

async fn prepare_handoff_plan_on_server(
    app: &mut App,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ops: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
    app_server: &mut AppServerSession,
    command: &str,
) -> Result<ThreadId> {
    let started = app_server.start_thread(&app.config).await?;
    let source_thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from(command));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppCommand::UserTurn { items, .. } = next_user_turn_op(ops) else {
        panic!("expected a handoff planning turn");
    };
    app.chat_widget
        .bind_handoff_turn_start("planning-turn", &items);
    app.chat_widget.handle_server_notification(
        turn_started_notification(source_thread_id, "planning-turn"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: source_thread_id.to_string(),
            turn_id: "planning-turn".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::Plan {
                id: "plan-1".to_string(),
                text: "- Finish the focused work".to_string(),
            },
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        turn_completed_notification(source_thread_id, "planning-turn", TurnStatus::Completed),
        /*replay_kind*/ None,
    );
    app.thread_event_channels[&source_thread_id]
        .store
        .lock()
        .await
        .latest_turn_id = Some("planning-turn".to_string());
    Ok(source_thread_id)
}

fn submit_deferred_prompt(
    app: &mut App,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    prompt: UserMessage,
) -> AppEvent {
    while events.try_recv().is_ok() {}
    app.chat_widget.restore_user_message_to_composer(prompt);
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
        .expect("next model-bound prompt should request deferred transfer")
}

#[path = "plan_handoff_deferred.rs"]
mod deferred;

#[tokio::test]
async fn accepted_steer_notification_invalidates_earlier_handoff_plan() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = app_server.start_thread(&app.config).await?;
    let source_thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while events.try_recv().is_ok() {}
    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("/handoff"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppCommand::UserTurn { items, .. } = next_user_turn_op(&mut ops) else {
        panic!("expected a handoff planning turn");
    };
    app.chat_widget
        .bind_handoff_turn_start("planning-turn", &items);
    app.chat_widget.handle_server_notification(
        turn_started_notification(source_thread_id, "planning-turn"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: source_thread_id.to_string(),
            turn_id: "planning-turn".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::Plan {
                id: "plan-before-steer".to_string(),
                text: "- Stale plan".to_string(),
            },
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: source_thread_id.to_string(),
            turn_id: "planning-turn".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::UserMessage {
                id: "accepted-steer".to_string(),
                client_id: None,
                content: vec![codex_app_server_protocol::UserInput::Text {
                    text: "Revise this first".to_string(),
                    text_elements: Vec::new(),
                }],
            },
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        turn_completed_notification(source_thread_id, "planning-turn", TurnStatus::Completed),
        /*replay_kind*/ None,
    );

    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
    );
    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    app_server.shutdown().await?;
    Ok(())
}

async fn prepared_handoff_app() -> Result<PreparedHandoffApp> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let tui = crate::tui::test_support::make_test_tui()?;
    let (mut app_server, _requests, _proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    Ok(PreparedHandoffApp {
        app,
        events,
        tui,
        app_server,
        source_thread_id,
        transfer,
    })
}

#[tokio::test]
async fn manual_handoff_starts_fresh_execution_and_preserves_source() -> Result<()> {
    let PreparedHandoffApp {
        mut app,
        mut events,
        mut tui,
        mut app_server,
        source_thread_id,
        transfer,
    } = prepared_handoff_app().await?;
    let original_model = app.chat_widget.current_model().to_string();
    // The Handoff Plan mask may use a different effort; the fresh Default turn
    // restores the source's selected Default-mode effort.
    let original_effort = Some(ReasoningEffortConfig::High);
    let original_service_tier = app.chat_widget.current_service_tier().map(str::to_string);
    let original_cwd = app.chat_widget.config_ref().cwd.clone();
    let original_approval = app
        .chat_widget
        .config_ref()
        .permissions
        .approval_policy
        .value();
    let original_profile = app
        .chat_widget
        .config_ref()
        .permissions
        .permission_profile()
        .clone();

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    let destination_thread_id = app.chat_widget.thread_id().expect("fresh thread");
    assert_ne!(destination_thread_id, source_thread_id);
    assert_eq!(app.chat_widget.current_model(), original_model);
    assert_eq!(app.chat_widget.current_reasoning_effort(), original_effort);
    assert_eq!(
        app.chat_widget.current_service_tier(),
        original_service_tier.as_deref()
    );
    assert_eq!(app.chat_widget.config_ref().cwd, original_cwd);
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .approval_policy
            .value(),
        original_approval,
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &original_profile,
    );
    assert!(
        app_server
            .thread_read(source_thread_id, /*include_turns*/ false)
            .await
            .is_ok()
    );
    let execution = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::CodexOp(AppCommand::UserTurn {
            items,
            collaboration_mode,
            ..
        }) => Some((items, collaboration_mode)),
        _ => None,
    });
    let (items, collaboration_mode) = execution.expect("fresh execution turn");
    assert_eq!(
        collaboration_mode.map(|mode| mode.mode),
        Some(ModeKind::Default)
    );
    assert_eq!(
        items,
        vec![codex_app_server_protocol::UserInput::Text {
            text: crate::handoff::HandoffPlan::new("- Finish the focused work".to_string())
                .expect("validated test plan")
                .execution_prompt(),
            text_elements: Vec::new(),
        }]
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn active_descendant_pauses_transfer_and_preserves_source() -> Result<()> {
    let PreparedHandoffApp {
        mut app,
        mut events,
        mut tui,
        mut app_server,
        source_thread_id,
        transfer,
    } = prepared_handoff_app().await?;
    let child_thread_id = ThreadId::new();
    app.agent_navigation.upsert(
        child_thread_id,
        /*agent_nickname*/ None,
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.agent_navigation.mark_running(child_thread_id);
    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    let message = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::InsertHistoryCell(cell) => {
            let text = lines_to_single_string(&cell.display_lines(/*width*/ 80));
            text.contains("Handoff transfer paused").then_some(text)
        }
        _ => None,
    });
    insta::assert_snapshot!(
        "handoff_active_descendant_gate",
        message.expect("pause notice")
    );

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn fresh_start_failure_keeps_the_source_thread_visible() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffConfigReadFails,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            codex_config::LoaderOverrides::default(),
        )
        .await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    let message = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::InsertHistoryCell(cell) => {
            let text = lines_to_single_string(&cell.display_lines(/*width*/ 80));
            text.contains("The source thread remains resumable")
                .then_some(text)
        }
        _ => None,
    });
    insta::assert_snapshot!(
        "handoff_fresh_start_failure",
        message.expect("failure notice")
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn handoff_thread_start_failure_keeps_source_and_does_not_execute() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffThreadStartFails,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn source_revision_change_during_start_pauses_transfer() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffSourceChangesAfterStart,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn newer_persisted_source_turn_pauses_transfer() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffLatestTurnDiffers,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn unavailable_source_turn_page_pauses_transfer() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let (source_thread_id, transfer) =
        prepare_handoff_on_server(&mut app, &mut events, &mut ops, &mut app_server).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(app.active_thread_id, Some(source_thread_id));
    let emitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
    );
    let message = emitted.into_iter().find_map(|event| match event {
        AppEvent::InsertHistoryCell(cell) => {
            let text = lines_to_single_string(&cell.display_lines(/*width*/ 80));
            text.contains("source Plan could not be verified")
                .then_some(text)
        }
        _ => None,
    });
    insta::assert_snapshot!(
        "handoff_unverifiable_source_plan",
        message.expect("pause notice")
    );
    app_server.shutdown().await?;
    Ok(())
}
