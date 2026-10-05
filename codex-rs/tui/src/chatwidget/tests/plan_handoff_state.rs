use pretty_assertions::assert_eq;

use super::*;

async fn complete_manual_handoff_plan(
    args: Option<&str>,
) -> (ChatWidget, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let (mut chat, events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    match args {
        Some(args) => {
            chat.dispatch_command_with_args(SlashCommand::Handoff, args.to_string(), Vec::new());
        }
        None => chat.dispatch_command(SlashCommand::Handoff),
    }
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected a handoff planning turn");
    };
    chat.bind_handoff_turn_start("planning-turn", &items);
    chat.on_plan_item_completed("- Finish the work".to_string(), "planning-turn".to_string());
    chat.note_handoff_turn_completed("planning-turn");
    chat.on_task_complete(
        /*last_agent_message*/ None, /*completion*/ None, /*from_replay*/ false,
    );
    (chat, events)
}

#[tokio::test]
async fn completed_default_handoff_emits_fresh_thread_transfer() {
    let (chat, mut events) = complete_manual_handoff_plan(/*args*/ None).await;
    let transfer = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::StartHandoffTransfer {
            source_thread_id,
            generation,
            plan,
        } => Some((source_thread_id, generation, plan)),
        _ => None,
    });
    let (source_thread_id, generation, plan) = transfer.expect("handoff transfer event");
    assert_eq!(source_thread_id, chat.thread_id.expect("source thread"));
    assert_eq!(plan, "- Finish the work");
    assert!(chat.handoff_transfer_is_locally_safe(source_thread_id, generation, &plan));
}

#[tokio::test]
async fn ask_handoff_shows_proceed_or_stay_without_transferring_early() {
    let (mut chat, mut events) = complete_manual_handoff_plan(Some("--ask")).await;
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
    );
    assert_chatwidget_snapshot!(
        "handoff_ask_decision",
        render_bottom_popup(&chat, /*width*/ 80),
    );
    assert_chatwidget_snapshot!(
        "handoff_ask_decision_narrow",
        render_bottom_popup(&chat, /*width*/ 40),
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let transfer = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::StartHandoffTransfer {
            source_thread_id,
            generation,
            plan,
        } => Some((source_thread_id, generation, plan)),
        _ => None,
    });
    let (source_thread_id, generation, plan) = transfer.expect("selected transfer event");
    assert!(chat.handoff_transfer_is_locally_safe(source_thread_id, generation, &plan));
}

#[tokio::test]
async fn ask_handoff_stay_keeps_source_mode_and_invalidates_old_transfer() {
    let (mut chat, mut events) = complete_manual_handoff_plan(Some("--ask")).await;
    let source_thread_id = chat.thread_id.expect("source thread");
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let generation = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::StayInHandoff {
            source_thread_id: event_thread_id,
            generation,
        } if event_thread_id == source_thread_id => Some(generation),
        _ => None,
    });
    let generation = generation.expect("stay event for source thread");
    chat.stay_in_handoff(generation);

    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
    assert!(!chat.handoff_transfer_is_locally_safe(
        source_thread_id,
        generation,
        "- Finish the work",
    ));
}

#[tokio::test]
async fn accepted_same_turn_steer_prevents_transfer_of_earlier_plan() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.dispatch_command(SlashCommand::Handoff);
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected planning turn");
    };
    chat.bind_handoff_turn_start("planning-turn", &items);
    chat.on_plan_item_completed("- Stale plan".to_string(), "planning-turn".to_string());
    chat.on_committed_user_message(
        &[UserInput::Text {
            text: "Revise this first".to_string(),
            text_elements: Vec::new(),
        }],
        Some("accepted-steer"),
        /*from_replay*/ false,
        "planning-turn",
    );
    chat.note_handoff_turn_completed("planning-turn");
    chat.on_task_complete(
        /*last_agent_message*/ None, /*completion*/ None, /*from_replay*/ false,
    );

    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
    );
    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
}

#[tokio::test]
async fn empty_handoff_plan_shows_validation_error_without_transfer() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.dispatch_command(SlashCommand::Handoff);
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected planning turn");
    };
    chat.bind_handoff_turn_start("planning-turn", &items);
    chat.on_plan_item_completed(String::new(), "planning-turn".to_string());
    chat.note_handoff_turn_completed("planning-turn");
    chat.on_task_complete(
        /*last_agent_message*/ None, /*completion*/ None, /*from_replay*/ false,
    );

    let history = drain_insert_history(&mut events)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("handoff_empty_plan", history);
    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
}

#[tokio::test]
async fn rejected_handoff_mode_update_restores_default_mode() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.dispatch_command(SlashCommand::Handoff);
    let _ = next_submit_op(&mut ops);
    let requested = chat.effective_collaboration_mode();

    chat.on_collaboration_mode_settings_update_failed(thread_id, &requested);

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    let history = drain_insert_history(&mut events)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("handoff_mode_rejected", history);
}
