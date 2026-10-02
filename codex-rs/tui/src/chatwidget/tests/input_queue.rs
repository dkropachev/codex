use super::*;
use pretty_assertions::assert_eq;
use std::collections::VecDeque;

#[tokio::test]
async fn interrupted_turn_restores_queued_messages_with_images_and_elements() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let first_placeholder = "[Image #1]";
    let first_text = format!("{first_placeholder} first");
    let first_elements = vec![TextElement::new(
        (0..first_placeholder.len()).into(),
        Some(first_placeholder.to_string()),
    )];
    let first_images = [PathBuf::from("/tmp/first.png")];

    let second_placeholder = "[Image #1]";
    let second_text = format!("{second_placeholder} second");
    let second_elements = vec![TextElement::new(
        (0..second_placeholder.len()).into(),
        Some(second_placeholder.to_string()),
    )];
    let second_images = [PathBuf::from("/tmp/second.png")];

    let existing_placeholder = "[Image #1]";
    let existing_text = format!("{existing_placeholder} existing");
    let existing_elements = vec![TextElement::new(
        (0..existing_placeholder.len()).into(),
        Some(existing_placeholder.to_string()),
    )];
    let existing_images = vec![PathBuf::from("/tmp/existing.png")];

    chat.input_queue.queued_user_messages.push_back(
        UserMessage {
            text: first_text,
            local_images: vec![LocalImageAttachment {
                placeholder: first_placeholder.to_string(),
                path: first_images[0].clone(),
            }],
            remote_image_urls: Vec::new(),
            text_elements: first_elements,
            mention_bindings: Vec::new(),
        }
        .into(),
    );
    chat.input_queue.queued_user_messages.push_back(
        UserMessage {
            text: second_text,
            local_images: vec![LocalImageAttachment {
                placeholder: second_placeholder.to_string(),
                path: second_images[0].clone(),
            }],
            remote_image_urls: Vec::new(),
            text_elements: second_elements,
            mention_bindings: Vec::new(),
        }
        .into(),
    );
    chat.refresh_pending_input_preview();

    chat.bottom_pane
        .set_composer_text(existing_text, existing_elements, existing_images.clone());

    // When interrupted, queued messages are merged into the composer; image placeholders
    // must be renumbered to match the combined local image list.
    handle_turn_interrupted(&mut chat, "turn-1");

    let first = "[Image #1] first".to_string();
    let second = "[Image #2] second".to_string();
    let third = "[Image #3] existing".to_string();
    let expected_text = format!("{first}\n{second}\n{third}");
    assert_eq!(chat.bottom_pane.composer_text(), expected_text);

    let first_start = 0;
    let second_start = first.len() + 1;
    let third_start = second_start + second.len() + 1;
    let expected_elements = vec![
        TextElement::new(
            (first_start..first_start + "[Image #1]".len()).into(),
            Some("[Image #1]".to_string()),
        ),
        TextElement::new(
            (second_start..second_start + "[Image #2]".len()).into(),
            Some("[Image #2]".to_string()),
        ),
        TextElement::new(
            (third_start..third_start + "[Image #3]".len()).into(),
            Some("[Image #3]".to_string()),
        ),
    ];
    assert_eq!(chat.bottom_pane.composer_text_elements(), expected_elements);
    assert_eq!(
        chat.bottom_pane.composer_local_image_paths(),
        vec![
            first_images[0].clone(),
            second_images[0].clone(),
            existing_images[0].clone(),
        ]
    );
}

#[tokio::test]
async fn restore_thread_input_state_restores_pending_steers_without_downgrading_them() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let mut pending_steers = VecDeque::new();
    pending_steers.push_back(UserMessage::from("pending steer"));
    let expected_compare_key = PendingSteerCompareKey {
        message: "hidden IDE context\npending steer".to_string(),
        image_count: 0,
    };
    let mut pending_steer_compare_keys = VecDeque::new();
    pending_steer_compare_keys.push_back(expected_compare_key.clone());
    let mut rejected_steers_queue = VecDeque::new();
    rejected_steers_queue.push_back(UserMessage::from("already rejected"));
    let mut queued_user_messages = VecDeque::new();
    queued_user_messages.push_back(UserMessage::from("queued draft").into());

    chat.restore_thread_input_state(
        Some(ThreadInputState {
            composer: None,
            safety_buffering_prompt: None,
            pending_steers,
            pending_steer_history_records: VecDeque::new(),
            pending_steer_compare_keys,
            rejected_steers_queue,
            rejected_steer_history_records: VecDeque::new(),
            queued_user_messages,
            queued_user_message_history_records: VecDeque::new(),
            recovered_queue: false,
            user_turn_pending_start: false,
            submit_pending_steers_after_interrupt: false,
            current_collaboration_mode: chat.current_collaboration_mode.clone(),
            active_collaboration_mask: chat.active_collaboration_mask.clone(),
            handoff_state: handoff::HandoffState::default(),
            task_running: false,
            agent_turn_running: false,
        }),
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: true,
            redisplay_pending_handoff: true,
        },
    );

    assert_eq!(
        chat.queued_user_message_texts(),
        vec!["already rejected", "queued draft"]
    );
    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "pending steer"
    );
    assert_eq!(
        chat.input_queue.pending_steers.front().unwrap().compare_key,
        expected_compare_key
    );
}

#[tokio::test]
async fn steer_enter_queues_while_plan_stream_is_active() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    let plan_mask = collaboration_modes::mask_for_kind(chat.model_catalog.as_ref(), ModeKind::Plan)
        .expect("expected plan collaboration mask");
    chat.set_collaboration_mask(plan_mask);
    chat.on_task_started();
    chat.on_plan_delta("- Step 1".to_string());
    let _ = drain_insert_history(&mut rx);

    chat.bottom_pane
        .set_composer_text("queued submission".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);
    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert_eq!(
        chat.input_queue.queued_user_messages.front().unwrap().text,
        "queued submission"
    );
    assert!(chat.input_queue.pending_steers.is_empty());
    assert_no_submit_op(&mut op_rx);
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn steer_enter_uses_pending_steers_while_turn_is_running_without_streaming() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();

    chat.bottom_pane
        .set_composer_text("queued while running".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "queued while running"
    );
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { .. } => {}
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }
    assert!(drain_insert_history(&mut rx).is_empty());

    complete_user_message(&mut chat, "user-1", "queued while running");

    assert!(chat.input_queue.pending_steers.is_empty());
    let inserted = drain_insert_history(&mut rx);
    assert_eq!(inserted.len(), 1);
    assert!(lines_to_single_string(&inserted[0]).contains("queued while running"));
}

#[tokio::test]
async fn steer_enter_uses_pending_steers_while_final_answer_stream_is_active() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    // Keep the assistant stream open (no commit tick/finalize) to model the repro window:
    // user presses Enter while the final answer is still streaming.
    chat.on_agent_message_delta("Final answer line\n".to_string());

    chat.bottom_pane.set_composer_text(
        "queued while streaming".to_string(),
        Vec::new(),
        Vec::new(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "queued while streaming"
    );
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { .. } => {}
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }
    assert!(drain_insert_history(&mut rx).is_empty());

    complete_user_message(&mut chat, "user-1", "queued while streaming");

    assert!(chat.input_queue.pending_steers.is_empty());
    let inserted = drain_insert_history(&mut rx);
    assert_eq!(inserted.len(), 1);
    assert!(lines_to_single_string(&inserted[0]).contains("queued while streaming"));
}

#[tokio::test]
async fn failed_pending_steer_submit_does_not_add_pending_preview() {
    let (mut chat, mut rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    drop(op_rx);

    chat.bottom_pane.set_composer_text(
        "queued while streaming".to_string(),
        Vec::new(),
        Vec::new(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(chat.input_queue.pending_steers.is_empty());
    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn item_completed_only_pops_front_pending_steer() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.input_queue
        .pending_steers
        .push_back(pending_steer("first"));
    chat.input_queue
        .pending_steers
        .push_back(pending_steer("second"));
    chat.refresh_pending_input_preview();

    complete_user_message(&mut chat, "user-other", "other");

    assert_eq!(chat.input_queue.pending_steers.len(), 2);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "first"
    );
    let inserted = drain_insert_history(&mut rx);
    assert_eq!(inserted.len(), 1);
    assert!(lines_to_single_string(&inserted[0]).contains("other"));

    complete_user_message(&mut chat, "user-first", "first");

    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "second"
    );
    let inserted = drain_insert_history(&mut rx);
    assert_eq!(inserted.len(), 1);
    assert!(lines_to_single_string(&inserted[0]).contains("first"));
}

#[tokio::test(flavor = "multi_thread")]
async fn item_completed_pops_pending_steer_with_local_image_and_text_elements() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();

    let temp = tempdir().expect("tempdir");
    let image_path = temp.path().join("pending-steer.png");
    const TINY_PNG_BYTES: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 11, 73, 68, 65, 84, 120, 156, 99, 96, 0, 2, 0, 0, 5, 0,
        1, 122, 94, 171, 63, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    std::fs::write(&image_path, TINY_PNG_BYTES).expect("write image");

    let text = "note".to_string();
    let text_elements = vec![TextElement::new((0..4).into(), Some("note".to_string()))];
    chat.submit_user_message(UserMessage {
        text: text.clone(),
        local_images: vec![LocalImageAttachment {
            placeholder: "[Image #1]".to_string(),
            path: image_path,
        }],
        remote_image_urls: Vec::new(),
        text_elements,
        mention_bindings: Vec::new(),
    });

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { .. } => {}
        other => panic!("expected Op::UserTurn, got {other:?}"),
    }

    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    let pending = chat.input_queue.pending_steers.front().unwrap();
    assert_eq!(pending.user_message.local_images.len(), 1);
    assert_eq!(pending.user_message.text_elements.len(), 1);
    assert_eq!(pending.compare_key.message, text);
    assert_eq!(pending.compare_key.image_count, 1);

    complete_user_message_for_inputs(
        &mut chat,
        "user-1",
        vec![
            UserInput::Image {
                url: "data:image/png;base64,placeholder".to_string(),
                detail: None,
            },
            UserInput::Text {
                text,
                text_elements: Vec::new(),
            },
        ],
    );

    assert!(chat.input_queue.pending_steers.is_empty());

    let mut user_cell = None;
    while let Ok(ev) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = ev
            && let Some(cell) = cell.as_any().downcast_ref::<UserHistoryCell>()
        {
            user_cell = Some((
                cell.message.clone(),
                cell.text_elements.clone(),
                cell.local_image_paths.clone(),
                cell.remote_image_urls.clone(),
            ));
            break;
        }
    }

    let (stored_message, stored_elements, stored_images, stored_remote_image_urls) =
        user_cell.expect("expected pending steer user history cell");
    assert_eq!(stored_message, "note");
    assert_eq!(
        stored_elements,
        vec![TextElement::new((0..4).into(), Some("note".to_string()))]
    );
    assert_eq!(stored_images.len(), 1);
    assert!(stored_images[0].ends_with("pending-steer.png"));
    assert!(stored_remote_image_urls.is_empty());
}

#[tokio::test]
async fn steer_enter_during_final_stream_preserves_follow_up_prompts_in_order() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();
    // Simulate "dead mode" repro timing by keeping a final-answer stream active while the
    // user submits multiple follow-up prompts.
    chat.on_agent_message_delta("Final answer line\n".to_string());

    chat.bottom_pane
        .set_composer_text("first follow-up".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    chat.bottom_pane
        .set_composer_text("second follow-up".to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(chat.input_queue.queued_user_messages.is_empty());
    assert_eq!(chat.input_queue.pending_steers.len(), 2);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "first follow-up"
    );
    assert_eq!(
        chat.input_queue
            .pending_steers
            .back()
            .unwrap()
            .user_message
            .text,
        "second follow-up"
    );

    let first_items = match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected Op::UserTurn, got {other:?}"),
    };
    assert_eq!(
        first_items,
        vec![UserInput::Text {
            text: "first follow-up".to_string(),
            text_elements: Vec::new(),
        }]
    );
    let second_items = match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected Op::UserTurn, got {other:?}"),
    };
    assert_eq!(
        second_items,
        vec![UserInput::Text {
            text: "second follow-up".to_string(),
            text_elements: Vec::new(),
        }]
    );
    assert!(drain_insert_history(&mut rx).is_empty());

    complete_user_message(&mut chat, "user-1", "first follow-up");

    assert_eq!(chat.input_queue.pending_steers.len(), 1);
    assert_eq!(
        chat.input_queue
            .pending_steers
            .front()
            .unwrap()
            .user_message
            .text,
        "second follow-up"
    );
    let first_insert = drain_insert_history(&mut rx);
    assert_eq!(first_insert.len(), 1);
    assert!(lines_to_single_string(&first_insert[0]).contains("first follow-up"));

    complete_user_message(&mut chat, "user-2", "second follow-up");

    assert!(chat.input_queue.pending_steers.is_empty());
    let second_insert = drain_insert_history(&mut rx);
    assert_eq!(second_insert.len(), 1);
    assert!(lines_to_single_string(&second_insert[0]).contains("second follow-up"));
}
