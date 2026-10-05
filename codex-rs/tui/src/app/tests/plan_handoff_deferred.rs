use pretty_assertions::assert_eq;

use super::*;

#[tokio::test]
async fn deferred_handoff_executes_plan_and_followup_in_fresh_thread() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Default
    );
    let transfer = submit_deferred_prompt(
        &mut app,
        &mut events,
        UserMessage::from("Run the final focused test"),
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_ne!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert!(!app.chat_widget.has_pending_deferred_handoff());
    let execution = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::CodexOp(AppCommand::UserTurn { items, .. }) => Some(items),
        _ => None,
    });
    let execution = execution.expect("fresh execution prompt");
    let [codex_app_server_protocol::UserInput::Text { text, .. }] = execution.as_slice() else {
        panic!("expected one merged execution prompt");
    };
    assert!(text.contains("- Finish the focused work"));
    assert!(text.ends_with("## Next user request\n\nRun the final focused test"));
    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("A later prompt"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
    assert!(
        app_server
            .thread_read(source_thread_id, /*include_turns*/ false)
            .await
            .is_ok()
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn deferred_handoff_preserves_followup_image_and_text_element() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    let url = "https://example.com/followup.png".to_string();
    let mut prompt = UserMessage::from("Inspect [Image #1]");
    prompt.remote_image_urls = vec![url.clone()];
    prompt.text_elements = vec![codex_protocol::user_input::TextElement::new(
        (8..18).into(),
        Some("[Image #1]".to_string()),
    )];
    let transfer = submit_deferred_prompt(&mut app, &mut events, prompt);
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    let items = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::CodexOp(AppCommand::UserTurn { items, .. }) => Some(items),
        _ => None,
    });
    let items = items.expect("fresh execution prompt");
    let [
        codex_app_server_protocol::UserInput::Image {
            image: codex_app_server_protocol::ImageReference::Inline { url: submitted_url },
            ..
        },
        codex_app_server_protocol::UserInput::Text {
            text,
            text_elements,
        },
    ] = items.as_slice()
    else {
        panic!("expected image and merged text");
    };
    assert_eq!(submitted_url, &url);
    let followup_start = text.find("Inspect [Image #1]").expect("followup text");
    assert_eq!(text_elements.len(), 1);
    assert_eq!(
        text_elements[0].byte_range,
        codex_app_server_protocol::ByteRange {
            start: followup_start + 8,
            end: followup_start + 18,
        }
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn deferred_start_failure_restores_prompt_and_pending_plan() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffConfigReadFails,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    let transfer =
        submit_deferred_prompt(&mut app, &mut events, UserMessage::from("Keep this draft"));
    let AppEvent::StartDeferredHandoffTransfer { generation, .. } = &transfer else {
        unreachable!("deferred transfer event");
    };
    let generation = *generation;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "Keep this draft"
    );
    assert!(
        app.chat_widget
            .is_current_deferred_transaction(source_thread_id, generation)
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn new_session_event_confirms_before_discarding_deferred_plan() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    while events.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::NewSession { name: None },
    )
    .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert!(app.chat_widget.has_pending_deferred_handoff());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.chat_widget.has_pending_deferred_handoff());

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::NewSession { name: None },
    )
    .await?;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let actions: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                event,
                AppEvent::DiscardDeferredHandoff { .. } | AppEvent::NewSession { .. }
            )
        })
        .collect();
    assert_eq!(actions.len(), 2);
    for action in actions {
        app.handle_event(&mut tui, &mut app_server, action).await?;
    }
    assert_ne!(app.chat_widget.thread_id(), Some(source_thread_id));
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn confirmed_inline_handoff_discards_pending_plan_and_preserves_arguments() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    while events.try_recv().is_ok() {}

    app.chat_widget.dispatch_command_with_args(
        SlashCommand::Handoff,
        "--ask keep the final check".to_string(),
        Vec::new(),
    );
    assert!(app.chat_widget.has_pending_deferred_handoff());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let confirmation = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::ConfirmDeferredHandoffDiscard { .. }))
        .expect("inline handoff discard confirmation");
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(&mut tui, &mut app_server, confirmation)
        .await?;

    assert!(!app.chat_widget.has_pending_deferred_handoff());
    let AppCommand::UserTurn { items, .. } = next_user_turn_op(&mut ops) else {
        panic!("confirmed inline handoff should submit a new Plan turn");
    };
    assert!(items.iter().any(|item| matches!(
        item,
        codex_app_server_protocol::UserInput::Text { text, .. }
            if text.contains("keep the final check")
    )));
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn prompt_edit_requires_confirmation_before_discarding_deferred_plan() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    let selected_cell: Arc<dyn HistoryCell> = Arc::new(history_cell::PlainHistoryCell::new(vec![
        "Earlier prompt".into(),
    ]));
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::RevertSessionForPromptEdit {
            thread_id: source_thread_id,
            selected_cell: Arc::clone(&selected_cell),
            prompt: UserMessage::from("Earlier prompt"),
        },
    )
    .await?;
    assert!(app.chat_widget.has_pending_deferred_handoff());
    insta::assert_snapshot!(
        "handoff_deferred_discard_prompt_edit",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.chat_widget.has_pending_deferred_handoff());

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::RevertSessionForPromptEdit {
            thread_id: source_thread_id,
            selected_cell,
            prompt: UserMessage::from("Earlier prompt"),
        },
    )
    .await?;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let actions: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                event,
                AppEvent::DiscardDeferredHandoff { .. }
                    | AppEvent::RevertSessionForPromptEdit { .. }
            )
        })
        .collect();
    assert_eq!(actions.len(), 2);
    for action in actions {
        if matches!(action, AppEvent::RevertSessionForPromptEdit { .. }) {
            break;
        }
        app.handle_event(&mut tui, &mut app_server, action).await?;
    }
    assert!(!app.chat_widget.has_pending_deferred_handoff());
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn deferred_plan_survives_thread_navigation_and_replay() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffPlanPage,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    app.thread_event_channels[&source_thread_id]
        .store
        .lock()
        .await
        .set_turns(vec![test_turn(
            "planning-turn",
            TurnStatus::Completed,
            vec![ThreadItem::Plan {
                id: "plan-1".to_string(),
                text: "- Finish the focused work".to_string(),
            }],
        )]);
    let other = app_server.start_thread(&app.config).await?;
    let other_thread_id = other.session.thread_id;
    app.thread_event_channels.insert(
        other_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            other.session,
            other.turns,
        ),
    );
    app.agent_navigation.upsert(
        other_thread_id,
        /*agent_nickname*/ None,
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.select_agent_thread(&mut tui, &mut app_server, other_thread_id)
        .await?;
    assert_eq!(app.chat_widget.thread_id(), Some(other_thread_id));
    app.select_agent_thread(&mut tui, &mut app_server, source_thread_id)
        .await?;

    assert_eq!(app.chat_widget.thread_id(), Some(source_thread_id));
    assert!(app.chat_widget.has_pending_deferred_handoff());
    let transfer = submit_deferred_prompt(
        &mut app,
        &mut events,
        UserMessage::from("Continue after navigation"),
    );
    assert!(matches!(
        transfer,
        AppEvent::StartDeferredHandoffTransfer { .. }
    ));
    proxy.abort();
    Ok(())
}

#[tokio::test]
async fn rejected_deferred_execution_restores_combined_prompt_without_resend() -> Result<()> {
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let (mut app_server, _requests, proxy) =
        session_lifecycle_requests::start_recording_app_server_with_history(
            &app.config,
            session_lifecycle_requests::HistoryCapabilities::HandoffTurnStartFails,
            /*blocked_thread_list*/ None,
            /*failed_thread_name*/ None,
            crate::app_server_session::ThreadParamsMode::Embedded,
            LoaderOverrides::default(),
        )
        .await?;
    let source_thread_id = prepare_handoff_plan_on_server(
        &mut app,
        &mut events,
        &mut ops,
        &mut app_server,
        "/handoff --defer",
    )
    .await?;
    let transfer = submit_deferred_prompt(
        &mut app,
        &mut events,
        UserMessage::from("Keep this follow-up"),
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_event(&mut tui, &mut app_server, transfer)
        .await?;
    assert_ne!(app.chat_widget.thread_id(), Some(source_thread_id));
    let execution = std::iter::from_fn(|| events.try_recv().ok())
        .find(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
        .expect("fresh execution turn");

    app.handle_event(&mut tui, &mut app_server, execution)
        .await?;

    let draft = app.chat_widget.composer_text_with_pending();
    assert!(draft.contains("- Finish the focused work"));
    assert!(draft.ends_with("## Next user request\n\nKeep this follow-up"));
    insta::assert_snapshot!(
        "handoff_deferred_rejected_execution",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(AppCommand::UserTurn { .. })))
    );
    proxy.abort();
    Ok(())
}
