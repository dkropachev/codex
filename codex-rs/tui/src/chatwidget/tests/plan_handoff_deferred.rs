use pretty_assertions::assert_eq;

use super::*;

async fn completed_deferred_plan() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.dispatch_command_with_args(SlashCommand::Handoff, "--defer".to_string(), Vec::new());
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected deferred handoff planning turn");
    };
    chat.bind_handoff_turn_start("deferred-plan-turn", &items);
    handle_turn_started(&mut chat, "deferred-plan-turn");
    chat.on_plan_item_completed(
        "- Finish after the next prompt".to_string(),
        "deferred-plan-turn".to_string(),
    );
    handle_turn_completed(&mut chat, "deferred-plan-turn", /*duration_ms*/ None);
    (chat, events, ops)
}

#[tokio::test]
async fn deferred_plan_waits_for_next_prompt_in_default_mode() {
    let (mut chat, mut events, mut ops) = completed_deferred_plan().await;
    let source_thread_id = chat.thread_id.expect("source thread");
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert_no_submit_op(&mut ops);
    let pending_notice = drain_insert_history(&mut events)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .find(|text| text.contains("Handoff plan ready for the next prompt"))
        .expect("pending handoff notice");
    insta::assert_snapshot!("handoff_deferred_pending", pending_notice);
    assert_chatwidget_snapshot!(
        "handoff_deferred_ready_footer",
        render_bottom_popup(&chat, /*width*/ 80),
    );

    chat.restore_user_message_to_composer(UserMessage::from("Check the final test"));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let transfer = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::StartDeferredHandoffTransfer {
            source_thread_id: event_source,
            plan_turn_id,
            generation,
            plan,
            user_message,
        } => Some((event_source, plan_turn_id, generation, plan, user_message)),
        _ => None,
    });
    let (event_source, plan_turn_id, generation, plan, user_message) =
        transfer.expect("deferred transfer event");
    assert_eq!(event_source, source_thread_id);
    assert_eq!(plan_turn_id, "deferred-plan-turn");
    assert_eq!(plan, "- Finish after the next prompt");
    assert_eq!(user_message, UserMessage::from("Check the final test"));
    assert!(chat.deferred_transfer_is_locally_safe(
        source_thread_id,
        generation,
        &plan,
        &user_message,
    ));
    assert_no_submit_op(&mut ops);
    assert_chatwidget_snapshot!(
        "handoff_deferred_starting_footer",
        render_bottom_popup(&chat, /*width*/ 80),
    );

    chat.rollback_deferred_handoff(generation);
    assert_eq!(chat.composer_text_with_pending(), "Check the final test");
    assert!(
        chat.handoff_state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.in_flight.is_none())
    );
}

#[tokio::test]
async fn shell_command_does_not_consume_deferred_handoff() {
    let (mut chat, mut events, mut ops) = completed_deferred_plan().await;
    while events.try_recv().is_ok() {}

    let accepted = chat.submit_user_message_with_history_record(
        UserMessage::from("!pwd"),
        UserMessageHistoryRecord::UserMessageText,
    );
    assert!(accepted);
    let emitted: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    let submitted_shell = matches!(ops.try_recv(), Ok(Op::RunUserShellCommand { .. }))
        || emitted
            .iter()
            .any(|event| matches!(event, AppEvent::CodexOp(Op::RunUserShellCommand { .. })));
    assert!(submitted_shell);
    assert!(chat.handoff_state.pending.is_some());
    assert!(
        !emitted
            .iter()
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
}

#[tokio::test]
async fn restored_deferred_transfer_keeps_prompt_in_recovered_queue() {
    let (mut chat, mut events, mut ops) = completed_deferred_plan().await;
    let source_thread_id = chat.thread_id.expect("source thread");
    while events.try_recv().is_ok() {}
    chat.restore_user_message_to_composer(UserMessage::from("Resume after reconnect"));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
    assert_no_submit_op(&mut ops);
    let input_state = chat.capture_thread_input_state();

    let (mut restored, mut restored_events, mut restored_ops) =
        make_chatwidget_manual(/*model_override*/ None).await;
    restored.thread_id = Some(source_thread_id);
    restored.restore_thread_input_state(
        input_state,
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
        },
    );

    assert!(
        restored
            .handoff_state
            .pending
            .as_ref()
            .is_some_and(|pending| pending.in_flight.is_none())
    );
    assert!(restored.input_queue.recovered_queue);
    assert_eq!(
        restored
            .input_queue
            .queued_user_messages
            .front()
            .map(|queued| queued.text.as_str()),
        Some("Resume after reconnect")
    );
    restored.maybe_send_next_queued_input();
    assert_no_submit_op(&mut restored_ops);
    assert!(
        !std::iter::from_fn(|| restored_events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
    assert_chatwidget_snapshot!(
        "handoff_deferred_recovered_queue",
        render_bottom_popup(&restored, /*width*/ 80),
    );
}

#[tokio::test]
async fn accepted_source_input_invalidates_pending_handoff_in_live_and_replay() {
    for from_replay in [false, true] {
        let (mut chat, mut events, _ops) = completed_deferred_plan().await;
        while events.try_recv().is_ok() {}

        chat.on_committed_user_message(
            &[UserInput::Text {
                text: "New source direction".to_string(),
                text_elements: Vec::new(),
            }],
            Some("external-client"),
            from_replay,
            "later-turn",
        );

        assert!(chat.handoff_state.pending.is_none());
        assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Handoff ready"));
        if !from_replay {
            let notice = drain_insert_history(&mut events)
                .into_iter()
                .map(|lines| lines_to_single_string(&lines))
                .find(|text| text.contains("deferred handoff plan was superseded"))
                .expect("superseded notice");
            insta::assert_snapshot!("handoff_deferred_superseded", notice);
        }
    }
}

#[tokio::test]
async fn destructive_commands_confirm_before_discarding_deferred_plan() {
    for command in [
        SlashCommand::Clear,
        SlashCommand::New,
        SlashCommand::Fork,
        SlashCommand::Resume,
        SlashCommand::Compact,
        SlashCommand::Archive,
        SlashCommand::Delete,
        SlashCommand::Quit,
    ] {
        let (mut chat, mut events, _ops) = completed_deferred_plan().await;
        while events.try_recv().is_ok() {}

        chat.dispatch_command(command);
        assert_chatwidget_snapshot!(
            format!("handoff_deferred_discard_{}", command.command()),
            render_bottom_popup(&chat, /*width*/ 80),
        );
        chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(chat.handoff_state.pending.is_some());
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, AppEvent::ConfirmDeferredHandoffDiscard { .. }))
        );

        chat.dispatch_command(command);
        chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let confirmed = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| {
            if let AppEvent::ConfirmDeferredHandoffDiscard { action, .. } = event {
                Some(action.command())
            } else {
                None
            }
        });
        assert_eq!(confirmed, Some(command));
        assert!(chat.handoff_state.pending.is_some());
    }
}

#[tokio::test]
async fn app_new_session_action_uses_the_same_discard_confirmation() {
    let (mut chat, mut events, _ops) = completed_deferred_plan().await;
    while events.try_recv().is_ok() {}

    assert!(chat.confirm_deferred_discard_app_action(
        "start a new session",
        Box::new(|tx| tx.send(AppEvent::NewSession { name: None })),
    ));
    assert_chatwidget_snapshot!(
        "handoff_deferred_discard_app_new",
        render_bottom_popup(&chat, /*width*/ 80),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let actions: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| {
            matches!(
                event,
                AppEvent::DiscardDeferredHandoff { .. } | AppEvent::NewSession { .. }
            )
        })
        .collect();
    assert_eq!(actions.len(), 2);
}

#[tokio::test]
async fn deferred_handoff_waits_for_remote_workspace_image_preparation() {
    let (mut chat, mut events, mut ops) = completed_deferred_plan().await;
    chat.snapshot_local_images = true;
    while events.try_recv().is_ok() {}
    let dir = tempfile::tempdir().unwrap();
    let image_path = dir.path().join("followup.png");
    image::RgbImage::new(/*width*/ 2, /*height*/ 2)
        .save(&image_path)
        .unwrap();
    let text = "Inspect [Image #1]";
    chat.bottom_pane.set_composer_text(
        text.to_string(),
        vec![TextElement::new(
            (8..18).into(),
            Some("[Image #1]".to_string()),
        )],
        vec![image_path.clone()],
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let image_id = loop {
        match events.recv().await.unwrap() {
            AppEvent::ImagesPrepared(id) => break id,
            AppEvent::StartDeferredHandoffTransfer { .. } => {
                panic!("handoff transferred before image preparation");
            }
            _ => {}
        }
    };
    chat.on_images_prepared(image_id);

    let submitted = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| {
        if let AppEvent::StartDeferredHandoffTransfer { user_message, .. } = event {
            Some(user_message)
        } else {
            None
        }
    });
    let submitted = submitted.expect("deferred transfer after image preparation");
    assert_eq!(submitted.text, text);
    assert_eq!(submitted.local_images[0].path, image_path);
    assert_no_submit_op(&mut ops);
}

#[tokio::test]
async fn reconnect_restores_pending_plan_without_automatic_transfer() {
    let (mut chat, mut events, _ops) = completed_deferred_plan().await;
    let source_thread_id = chat.thread_id.expect("source thread");
    while events.try_recv().is_ok() {}
    chat.pause_for_disconnect();
    let input_state = chat.capture_thread_input_state();

    let (mut restored, mut restored_events, mut restored_ops) =
        make_chatwidget_manual(/*model_override*/ None).await;
    restored.thread_id = Some(source_thread_id);
    restored.restore_reconnected_input(input_state);
    restored.on_plan_item_completed(
        "- Finish after the next prompt".to_string(),
        "deferred-plan-turn".to_string(),
    );

    assert!(restored.has_pending_deferred_handoff());
    assert!(render_bottom_popup(&restored, /*width*/ 80).contains("Handoff ready"));
    assert_no_submit_op(&mut restored_ops);
    assert!(
        !std::iter::from_fn(|| restored_events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartDeferredHandoffTransfer { .. }))
    );
}

#[tokio::test]
async fn rejected_default_mode_update_cancels_deferred_plan() {
    let (mut chat, mut events, _ops) = completed_deferred_plan().await;
    let source_thread_id = chat.thread_id.expect("source thread");
    let requested = chat.effective_collaboration_mode();
    while events.try_recv().is_ok() {}

    chat.on_collaboration_mode_settings_update_failed(source_thread_id, &requested);

    assert!(!chat.has_pending_deferred_handoff());
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Handoff ready"));
    let notice = drain_insert_history(&mut events)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .find(|text| text.contains("Deferred handoff could not return to Default mode"))
        .expect("mode failure notice");
    insta::assert_snapshot!("handoff_deferred_mode_rejected", notice);
}
