use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn manual_interrupt_restores_pending_steers_to_composer() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    chat.on_agent_message_delta(
        "Final answer line
"
        .to_string(),
    );

    chat.bottom_pane.set_composer_text(
        "queued while streaming".to_string(),
        Vec::new(),
        Vec::new(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued while streaming".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.on_interrupted_turn(TurnAbortReason::Interrupted);

    assert!(chat.input_queue.pending_steers.is_empty());
    assert_eq!(chat.bottom_pane.composer_text(), "queued while streaming");
    assert_no_submit_op(&mut op_rx);

    let inserted = drain_insert_history(&mut rx);
    assert!(
        inserted
            .iter()
            .all(|cell| !lines_to_single_string(cell).contains("queued while streaming"))
    );
}

#[tokio::test]
async fn esc_interrupt_sends_all_pending_steers_immediately_and_keeps_existing_draft() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    chat.on_agent_message_delta("Final answer line\n".to_string());

    chat.bottom_pane
        .set_composer_text("first pending steer".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "first pending steer".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }

    chat.bottom_pane
        .set_composer_text("second pending steer".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "second pending steer".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }

    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("queued draft".to_string()).into());
    chat.refresh_pending_input_preview();
    chat.bottom_pane
        .set_composer_text("still editing".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    next_interrupt_op(&mut op_rx);

    chat.on_interrupted_turn(TurnAbortReason::Interrupted);

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "first pending steer\nsecond pending steer".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected merged pending steers to submit, got {other:?}"),
    }

    assert!(chat.input_queue.pending_steers.is_empty());
    assert_eq!(chat.bottom_pane.composer_text(), "still editing");
    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert_eq!(
        chat.input_queue.queued_user_messages.front().unwrap().text,
        "queued draft"
    );

    let inserted = drain_insert_history(&mut rx);
    assert!(
        inserted
            .iter()
            .any(|cell| lines_to_single_string(cell).contains("first pending steer"))
    );
    assert!(
        inserted
            .iter()
            .any(|cell| lines_to_single_string(cell).contains("second pending steer"))
    );
}

#[tokio::test]
async fn esc_with_pending_steers_overrides_agent_command_interrupt_behavior() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();

    chat.bottom_pane
        .set_composer_text("pending steer".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { .. } => {}
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }

    chat.bottom_pane
        .set_composer_text("/subagents ".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    next_interrupt_op(&mut op_rx);
    assert_eq!(chat.bottom_pane.composer_text(), "/subagents ");
}

#[tokio::test]
async fn manual_interrupt_restores_pending_steer_mention_bindings_to_composer() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    chat.on_agent_message_delta("Final answer line\n".to_string());

    let mention_bindings = vec![MentionBinding {
        sigil: '$',
        mention: "figma".to_string(),
        path: "/tmp/skills/figma/SKILL.md".to_string(),
    }];
    chat.bottom_pane.set_composer_text_with_mention_bindings(
        "please use $figma".to_string(),
        vec![TextElement::new(
            (11..17).into(),
            Some("$figma".to_string()),
        )],
        Vec::new(),
        mention_bindings.clone(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "please use $figma".to_string(),
                text_elements: vec![
                    TextElement::new((11..17).into(), Some("$figma".to_string())).into()
                ],
            }]
        ),
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }

    chat.on_interrupted_turn(TurnAbortReason::Interrupted);

    assert_eq!(chat.bottom_pane.composer_text(), "please use $figma");
    assert_eq!(chat.bottom_pane.take_mention_bindings(), mention_bindings);
    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn manual_interrupt_restores_pending_steers_before_queued_messages() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    chat.on_agent_message_delta(
        "Final answer line
"
        .to_string(),
    );

    chat.bottom_pane
        .set_composer_text("pending steer".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("queued draft".to_string()).into());
    chat.refresh_pending_input_preview();

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "pending steer".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }
    assert!(drain_insert_history(&mut rx).is_empty());

    chat.on_interrupted_turn(TurnAbortReason::Interrupted);

    assert!(chat.input_queue.pending_steers.is_empty());
    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "pending steer
queued draft"
    );
    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn ctrl_c_shutdown_works_with_caps_lock() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL));

    assert_matches!(rx.try_recv(), Ok(AppEvent::Exit(ExitMode::ShutdownFirst)));
}

#[tokio::test]
async fn ctrl_c_interrupts_without_arming_quit_when_double_press_disabled() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.bottom_pane.set_task_running(/*running*/ true);

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    next_interrupt_op(&mut op_rx);
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    next_interrupt_op(&mut op_rx);
    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());
}

#[tokio::test]
async fn ctrl_c_cleared_prompt_is_recoverable_via_history() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.bottom_pane.insert_str("draft message ");
    chat.bottom_pane
        .attach_image(PathBuf::from("/tmp/preview.png"));
    let placeholder = "[Image #1]";
    assert!(
        chat.bottom_pane.composer_text().ends_with(placeholder),
        "expected placeholder {placeholder:?} in composer text"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(chat.bottom_pane.composer_text().is_empty());
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());

    chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    let restored_text = chat.bottom_pane.composer_text();
    assert!(
        restored_text.ends_with(placeholder),
        "expected placeholder {placeholder:?} after history recall"
    );
    assert!(restored_text.starts_with("draft message "));
    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());

    let images = chat.bottom_pane.take_recent_submission_images();
    assert_eq!(vec![PathBuf::from("/tmp/preview.png")], images);
}

// Snapshot test: interrupting a running exec finalizes the active cell with a red ✗
// marker (replacing the spinner) and flushes it into history.
#[tokio::test]
async fn interrupt_exec_marks_failed_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    // Begin a long-running command so we have an active exec cell with a spinner.
    begin_exec(&mut chat, "call-int", "sleep 1");

    // Simulate the task being aborted (as if ESC was pressed), which should
    // cause the active exec cell to be finalized as failed and flushed.
    handle_turn_interrupted(&mut chat, "turn-1");

    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected finalized exec cell to be inserted into history"
    );

    // The first inserted cell should be the finalized exec; snapshot its text.
    let exec_blob = lines_to_single_string(&cells[0]);
    assert_chatwidget_snapshot!("interrupt_exec_marks_failed", exec_blob);
}

// Snapshot test: after an interrupted turn, a gentle error message is inserted
// suggesting the user to tell the model what to do differently and to use /feedback.
#[tokio::test]
async fn interrupted_turn_error_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    // Simulate an in-progress task so the widget is in a running state.
    handle_turn_started(&mut chat, "turn-1");

    // Abort the turn (like pressing Esc) and drain inserted history.
    handle_turn_interrupted(&mut chat, "turn-1");

    let cells = drain_insert_history(&mut rx);
    assert!(
        !cells.is_empty(),
        "expected error message to be inserted after interruption"
    );
    let last = lines_to_single_string(cells.last().unwrap());
    assert_chatwidget_snapshot!("interrupted_turn_error_message", last);
}

#[tokio::test]
async fn interrupted_turn_after_goal_budget_limited_uses_budget_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Goals, /*enabled*/ true);

    chat.handle_server_notification(
        codex_app_server_protocol::ServerNotification::TurnStarted(
            codex_app_server_protocol::TurnStartedNotification {
                thread_id: "thread-1".to_string(),
                turn: codex_app_server_protocol::Turn {
                    id: "turn-1".to_string(),
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: Vec::new(),
                    status: codex_app_server_protocol::TurnStatus::InProgress,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            },
        ),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        codex_app_server_protocol::ServerNotification::ThreadGoalUpdated(
            codex_app_server_protocol::ThreadGoalUpdatedNotification {
                thread_id: "thread-1".to_string(),
                turn_id: Some("turn-1".to_string()),
                goal: codex_app_server_protocol::ThreadGoal {
                    thread_id: "thread-1".to_string(),
                    objective: "Run until the token budget is limited".to_string(),
                    status: codex_app_server_protocol::ThreadGoalStatus::BudgetLimited,
                    token_budget: Some(10_000),
                    tokens_used: 10_500,
                    time_used_seconds: 0,
                    created_at: 0,
                    updated_at: 1,
                },
            },
        ),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        codex_app_server_protocol::ServerNotification::TurnCompleted(
            codex_app_server_protocol::TurnCompletedNotification {
                thread_id: "thread-1".to_string(),
                turn: codex_app_server_protocol::Turn {
                    id: "turn-1".to_string(),
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: Vec::new(),
                    status: codex_app_server_protocol::TurnStatus::Interrupted,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            },
        ),
        /*replay_kind*/ None,
    );

    let cells = drain_insert_history(&mut rx);
    let last = lines_to_single_string(cells.last().unwrap());
    assert_chatwidget_snapshot!("interrupted_turn_goal_budget_limited_message", last);
}

#[tokio::test]
async fn direct_budget_limited_turn_uses_budget_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    handle_turn_started(&mut chat, "turn-1");
    handle_budget_limited_turn(&mut chat, "turn-1");

    let cells = drain_insert_history(&mut rx);
    let last = lines_to_single_string(cells.last().unwrap());
    assert_chatwidget_snapshot!("direct_budget_limited_turn_message", last);
}

#[tokio::test]
async fn budget_limited_turn_restores_queued_input_without_submitting() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("follow-up after budget stop").into());
    chat.refresh_pending_input_preview();

    handle_turn_started(&mut chat, "turn-1");
    handle_budget_limited_turn(&mut chat, "turn-1");

    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert_eq!(
        chat.bottom_pane.composer_text(),
        "follow-up after budget stop"
    );
    assert_no_submit_op(&mut op_rx);
}

// Snapshot test: interrupting specifically to submit pending steers shows an
// informational banner instead of the generic "tell the model what to do
// differently" error prompt.
#[tokio::test]
async fn interrupted_turn_pending_steers_message_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.input_queue
        .pending_steers
        .push_back(pending_steer("steer 1"));
    chat.input_queue.submit_pending_steers_after_interrupt = true;

    handle_turn_started(&mut chat, "turn-1");

    handle_turn_interrupted(&mut chat, "turn-1");

    let cells = drain_insert_history(&mut rx);
    let info = cells
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .find(|line| line.contains("Model interrupted to submit steer instructions."))
        .expect("expected steer interrupt info message to be inserted");
    assert_chatwidget_snapshot!("interrupted_turn_pending_steers_message", info);
}
