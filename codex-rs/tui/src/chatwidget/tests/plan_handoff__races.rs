use super::*;
use crate::handoff::HandoffDisposition;
use crate::handoff::HandoffTrigger;
use crate::handoff::PendingHandoffPlan;
use pretty_assertions::assert_eq;

async fn configured_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    (chat, rx, op_rx)
}

fn usage_info(last_tokens: i64) -> TokenUsageInfo {
    TokenUsageInfo {
        total_token_usage: TokenUsage {
            total_tokens: 400_000,
            ..TokenUsage::default()
        },
        last_token_usage: TokenUsage {
            total_tokens: last_tokens,
            ..TokenUsage::default()
        },
        model_context_window: Some(100_000),
    }
}

async fn latched_automatic_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, rx, op_rx) = configured_chat().await;
    chat.config.tui_auto_handoff_threshold_percent = Some(71);
    handle_turn_started(&mut chat, "threshold-turn");
    handle_token_count(&mut chat, Some(usage_info(/*last_tokens*/ 74_480)));
    handle_turn_completed(&mut chat, "threshold-turn", /*duration_ms*/ None);
    assert!(chat.automatic_handoff_is_locally_eligible());
    (chat, rx, op_rx)
}

fn take_user_turn(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Vec<UserInput> {
    match next_submit_op(op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected user turn, got {other:?}"),
    }
}

fn commit_user_turn(chat: &mut ChatWidget, turn_id: &str, items: Vec<UserInput>) {
    chat.bind_handoff_turn_start(turn_id, &items);
    handle_turn_started(chat, turn_id);
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.map(|id| id.to_string()).unwrap_or_default(),
            turn_id: turn_id.to_string(),
            item: ThreadItem::UserMessage {
                id: format!("{turn_id}-user"),
                client_id: None,
                content: items,
            },
            completed_at_ms: 0,
        }),
        /*replay_kind*/ None,
    );
}

fn take_planning_gate(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> Option<u64> {
    std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::AdvanceAutomaticHandoffPlanning { generation, .. } => Some(generation),
        _ => None,
    })
}

fn emit_compaction(chat: &mut ChatWidget, id: &str) {
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.expect("configured thread").to_string(),
            turn_id: "threshold-turn".to_string(),
            item: ThreadItem::ContextCompaction { id: id.to_string() },
            completed_at_ms: 0,
        }),
        /*replay_kind*/ None,
    );
}

#[tokio::test]
async fn plan_handoff_automatic_ignores_stale_completion_and_rechecks_planning_gap() {
    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    while rx.try_recv().is_ok() {}
    assert!(chat.start_automatic_handoff());
    let wrap_up_items = take_user_turn(&mut op_rx);

    handle_turn_started(&mut chat, "unrelated-turn");
    handle_turn_completed(&mut chat, "unrelated-turn", /*duration_ms*/ None);
    assert_eq!(take_planning_gate(&mut rx), None);
    assert_no_submit_op(&mut op_rx);

    commit_user_turn(&mut chat, "wrap-up-turn", wrap_up_items);
    handle_turn_completed(&mut chat, "wrap-up-turn", /*duration_ms*/ None);
    let generation = take_planning_gate(&mut rx).expect("owned wrap-up should reach app gate");

    chat.restore_user_message_to_composer(UserMessage::from("new draft during gate"));
    assert!(!chat.continue_automatic_handoff_planning(generation));
    assert_no_submit_op(&mut op_rx);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn plan_handoff_turn_start_response_beats_identical_stale_user_item() {
    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    while rx.try_recv().is_ok() {}
    assert!(chat.start_automatic_handoff());
    let wrap_up_items = take_user_turn(&mut op_rx);
    chat.bind_handoff_turn_start("owned-wrap-up", &wrap_up_items);

    handle_turn_started(&mut chat, "stale-wrap-up");
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.expect("configured thread").to_string(),
            turn_id: "stale-wrap-up".to_string(),
            item: ThreadItem::UserMessage {
                id: "stale-wrap-up-user".to_string(),
                client_id: None,
                content: wrap_up_items.clone(),
            },
            completed_at_ms: 0,
        }),
        /*replay_kind*/ None,
    );
    handle_turn_completed(&mut chat, "stale-wrap-up", /*duration_ms*/ None);
    assert_eq!(take_planning_gate(&mut rx), None);

    handle_turn_started(&mut chat, "owned-wrap-up");
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.expect("configured thread").to_string(),
            turn_id: "owned-wrap-up".to_string(),
            item: ThreadItem::UserMessage {
                id: "owned-wrap-up-user".to_string(),
                client_id: None,
                content: wrap_up_items,
            },
            completed_at_ms: 0,
        }),
        /*replay_kind*/ None,
    );
    handle_turn_completed(&mut chat, "owned-wrap-up", /*duration_ms*/ None);
    assert!(take_planning_gate(&mut rx).is_some());
}

#[tokio::test]
async fn plan_handoff_lost_start_response_does_not_reuse_older_identical_plan() {
    let expected_items = vec![UserInput::Text {
        text: crate::handoff::manual_planning_prompt(""),
        text_elements: Vec::new(),
    }];
    let mut old_handoff = app_server_turn(
        "old-identical-handoff",
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    old_handoff.items = vec![
        ThreadItem::UserMessage {
            id: "old-identical-user".to_string(),
            client_id: None,
            content: expected_items.clone(),
        },
        ThreadItem::Plan {
            id: "old-identical-plan".to_string(),
            text: "- Do not reuse this old plan".to_string(),
        },
    ];
    let boundary = app_server_turn(
        "submission-boundary",
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    let turns = vec![old_handoff, boundary];
    let (mut chat, _rx, mut op_rx) = configured_chat().await;
    chat.replay_thread_turns(turns.clone(), ReplayKind::ThreadSnapshot);
    chat.dispatch_command(SlashCommand::Handoff);
    let planning_items = take_user_turn(&mut op_rx);
    assert_eq!(planning_items, expected_items);
    let mut input_state = chat
        .capture_thread_input_state()
        .expect("handoff submission state");
    input_state.reconcile_handoff_turns(&turns);

    let (mut restored, mut restored_rx, _restored_ops) = configured_chat().await;
    restored.replay_thread_turns(turns, ReplayKind::ThreadSnapshot);
    restored.restore_thread_input_state(
        Some(input_state),
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
            redisplay_pending_handoff: false,
        },
    );
    restored.advance_restored_manual_handoff_after_snapshot();
    assert!(
        !std::iter::from_fn(|| restored_rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
    );
}

#[tokio::test]
async fn plan_handoff_accepted_steer_rebinds_the_running_owned_turn() {
    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    while rx.try_recv().is_ok() {}
    assert!(chat.start_automatic_handoff());
    let wrap_up_items = take_user_turn(&mut op_rx);
    chat.bind_handoff_turn_start("wrap-up-turn", &wrap_up_items);
    handle_turn_started(&mut chat, "wrap-up-turn");

    chat.submit_user_message(UserMessage::from("Include the latest validation result."));
    let steer_items = take_user_turn(&mut op_rx);
    chat.bind_handoff_turn_start("wrap-up-turn", &steer_items);
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.expect("configured thread").to_string(),
            turn_id: "wrap-up-turn".to_string(),
            item: ThreadItem::UserMessage {
                id: "wrap-up-steer".to_string(),
                client_id: None,
                content: steer_items,
            },
            completed_at_ms: 0,
        }),
        /*replay_kind*/ None,
    );
    handle_turn_completed(&mut chat, "wrap-up-turn", /*duration_ms*/ None);

    assert!(take_planning_gate(&mut rx).is_some());
}

#[tokio::test]
async fn plan_handoff_response_lost_steer_reconciles_inside_the_running_turn() {
    let (mut source, _source_rx, mut source_ops) = configured_chat().await;
    source.dispatch_command(SlashCommand::Handoff);
    let planning_items = take_user_turn(&mut source_ops);
    source.bind_handoff_turn_start("planning-turn", &planning_items);
    handle_turn_started(&mut source, "planning-turn");
    source.note_handoff_submission(vec![UserInput::Text {
        text: "response-lost steer".to_string(),
        text_elements: Vec::new(),
    }]);
    let mut input_state = source
        .capture_thread_input_state()
        .expect("manual handoff input state");

    let mut completed = app_server_turn(
        "planning-turn",
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    completed.items = vec![
        ThreadItem::UserMessage {
            id: "response-lost-steer".to_string(),
            client_id: None,
            content: vec![UserInput::Text {
                text: "response-lost steer".to_string(),
                text_elements: Vec::new(),
            }],
        },
        ThreadItem::Plan {
            id: "response-lost-plan".to_string(),
            text: "- Continue after the recovered steer".to_string(),
        },
    ];
    let turns = vec![completed];
    input_state.reconcile_handoff_turns(&turns);

    let (mut restored, mut restored_rx, _restored_ops) = configured_chat().await;
    restored.replay_thread_turns(turns, ReplayKind::ThreadSnapshot);
    restored.restore_thread_input_state(
        Some(input_state),
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
            redisplay_pending_handoff: false,
        },
    );
    restored.advance_restored_manual_handoff_after_snapshot();
    assert!(
        std::iter::from_fn(|| restored_rx.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::StartHandoffTransfer { plan, .. }
                if plan == "- Continue after the recovered steer"
        ))
    );
}

#[tokio::test]
async fn plan_handoff_rejected_steer_queue_cancels_before_ownership_check() {
    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    while rx.try_recv().is_ok() {}
    assert!(chat.start_automatic_handoff());
    let wrap_up_items = take_user_turn(&mut op_rx);
    chat.bind_handoff_turn_start("wrap-up-turn", &wrap_up_items);
    handle_turn_started(&mut chat, "wrap-up-turn");

    chat.note_handoff_submission(vec![UserInput::Text {
        text: "a rejected steer".to_string(),
        text_elements: Vec::new(),
    }]);
    chat.input_queue
        .queued_user_messages
        .push_back(UserMessage::from("a rejected steer").into());
    handle_turn_completed(&mut chat, "wrap-up-turn", /*duration_ms*/ None);

    assert_eq!(take_planning_gate(&mut rx), None);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn plan_handoff_compaction_preserves_high_latch_and_rearms_after_real_drop() {
    let (mut still_high, _rx, _op_rx) = latched_automatic_chat().await;
    emit_compaction(&mut still_high, "still-high");
    assert!(still_high.automatic_handoff_is_locally_eligible());

    let (mut dropped, _rx, _op_rx) = latched_automatic_chat().await;
    handle_token_count(&mut dropped, Some(usage_info(/*last_tokens*/ 73_600)));
    emit_compaction(&mut dropped, "dropped-below-auto-threshold");
    assert!(!dropped.automatic_handoff_is_locally_eligible());

    handle_turn_started(&mut dropped, "post-compaction-turn");
    handle_token_count(&mut dropped, Some(usage_info(/*last_tokens*/ 74_480)));
    handle_turn_completed(
        &mut dropped,
        "post-compaction-turn",
        /*duration_ms*/ None,
    );
    assert!(dropped.automatic_handoff_is_locally_eligible());
}

#[tokio::test]
async fn plan_handoff_rate_limit_unblock_reissues_the_latched_candidate() {
    let (mut chat, mut rx, _op_rx) = latched_automatic_chat().await;
    while rx.try_recv().is_ok() {}
    chat.rate_limit_switch_prompt = RateLimitSwitchPromptState::Pending;
    let mut funded = snapshot(/*percent*/ 95.0);
    funded.credits = Some(CreditsSnapshot {
        has_credits: true,
        unlimited: false,
        balance: None,
    });

    chat.on_rate_limit_snapshot(Some(funded));

    assert!(matches!(
        chat.rate_limit_switch_prompt,
        RateLimitSwitchPromptState::Idle
    ));
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::AutomaticHandoffCandidate { .. }))
    );
}

#[tokio::test]
async fn plan_handoff_replayed_low_usage_rearms_the_one_shot_hint_only() {
    let (mut chat, mut rx, _op_rx) = configured_chat().await;
    handle_token_count(&mut chat, Some(usage_info(/*last_tokens*/ 73_600)));
    assert!(
        drain_insert_history(&mut rx)
            .iter()
            .any(|lines| lines_to_single_string(lines).contains("Context is getting full."))
    );

    chat.set_token_info(Some(usage_info(/*last_tokens*/ 72_720)));
    chat.observe_handoff_context_usage(/*from_replay*/ true);
    handle_token_count(&mut chat, Some(usage_info(/*last_tokens*/ 73_600)));
    assert!(
        drain_insert_history(&mut rx)
            .iter()
            .any(|lines| lines_to_single_string(lines).contains("Context is getting full."))
    );
}

#[tokio::test]
async fn plan_handoff_stay_rotates_generation_and_rejects_stale_transfer() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    chat.dispatch_command_with_args(
        SlashCommand::Handoff,
        "--ask keep it focused".to_string(),
        Vec::new(),
    );
    let planning_items = take_user_turn(&mut op_rx);
    commit_user_turn(&mut chat, "ask-plan", planning_items);
    chat.on_plan_item_completed("- Continue safely".to_string());
    handle_turn_completed(&mut chat, "ask-plan", /*duration_ms*/ None);
    while rx.try_recv().is_ok() {}

    let source_thread_id = chat.thread_id.expect("configured thread");
    let stale_generation = chat
        .active_handoff_generation()
        .expect("ask handoff generation");
    chat.stay_in_handoff(stale_generation, HandoffTrigger::Manual);
    assert_ne!(chat.active_handoff_generation(), Some(stale_generation));
    assert!(!chat.handoff_transfer_is_locally_safe(
        source_thread_id,
        stale_generation,
        HandoffTrigger::Manual,
        HandoffDisposition::Proceed,
    ));
}

#[tokio::test]
async fn plan_handoff_reattach_does_not_reuse_a_cancelled_generation() {
    let (mut source, _source_rx, mut source_ops) = configured_chat().await;
    source.dispatch_command(SlashCommand::Handoff);
    let _ = take_user_turn(&mut source_ops);
    let stale_generation = source
        .active_handoff_generation()
        .expect("first handoff generation");
    source.set_collaboration_mask_from_user_action(
        collaboration_modes::default_mode_mask(source.model_catalog.as_ref())
            .expect("default mode"),
    );
    let passive = source.passive_handoff_state();

    let (mut restored, _restored_rx, mut restored_ops) = configured_chat().await;
    restored.restore_passive_handoff_state(passive);
    restored.dispatch_command(SlashCommand::Handoff);
    let _ = take_user_turn(&mut restored_ops);
    let new_generation = restored
        .active_handoff_generation()
        .expect("reattached handoff generation");
    assert!(new_generation > stale_generation);
    assert!(!restored.handoff_transfer_is_locally_safe(
        restored.thread_id.expect("configured thread"),
        stale_generation,
        HandoffTrigger::Manual,
        HandoffDisposition::Proceed,
    ));
}

#[tokio::test]
async fn plan_handoff_reconciles_manual_plan_completed_while_detached() {
    let (mut source, _source_rx, mut source_ops) = configured_chat().await;
    source.dispatch_command(SlashCommand::Handoff);
    let planning_items = take_user_turn(&mut source_ops);
    let mut input_state = source
        .capture_thread_input_state()
        .expect("manual handoff state should be captured");

    let mut completed = app_server_turn(
        "detached-plan",
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    completed.items = vec![
        ThreadItem::UserMessage {
            id: "detached-plan-user".to_string(),
            client_id: None,
            content: planning_items,
        },
        ThreadItem::Plan {
            id: "detached-plan-item".to_string(),
            text: "- Resume from the detached plan".to_string(),
        },
    ];
    let turns = vec![completed];
    input_state.reconcile_handoff_turns(&turns);

    let (mut restored, mut restored_rx, _restored_ops) = configured_chat().await;
    restored.replay_thread_turns(turns, ReplayKind::ThreadSnapshot);
    restored.restore_thread_input_state(
        Some(input_state),
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
            redisplay_pending_handoff: false,
        },
    );
    restored.advance_restored_manual_handoff_after_snapshot();

    let transfer =
        std::iter::from_fn(|| restored_rx.try_recv().ok()).find_map(|event| match event {
            AppEvent::StartHandoffTransfer {
                plan,
                disposition,
                trigger,
                ..
            } => Some((plan, disposition, trigger)),
            _ => None,
        });
    assert_eq!(
        transfer,
        Some((
            "- Resume from the detached plan".to_string(),
            HandoffDisposition::Proceed,
            HandoffTrigger::Manual,
        ))
    );
}

#[tokio::test]
async fn plan_handoff_proceed_rejection_restores_a_visible_pending_plan() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    let pending = PendingHandoffPlan::new("- Retry execution safely".to_string())
        .expect("valid handoff plan");
    assert!(chat.submit_handoff_execution(pending, HandoffTrigger::Manual));
    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });

    assert!(chat.handle_turn_start_rejection("turn/start rejected".to_string()));

    let pending = chat
        .pending_handoff_state()
        .expect("rejected proceed should remain pending");
    assert_eq!(pending.plan().plan(), "- Retry execution safely");
    assert_eq!(pending.submitted_text(), None);
    let history = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(history.contains("Pending handoff plan"));
    assert!(history.contains("Retry execution safely"));
}

#[tokio::test]
async fn plan_handoff_disconnect_does_not_expand_proceed_prompt_twice() {
    let (mut source, _source_rx, mut source_ops) = configured_chat().await;
    let pending =
        PendingHandoffPlan::new("- Resume exactly once".to_string()).expect("valid handoff plan");
    assert!(source.submit_handoff_execution(pending.clone(), HandoffTrigger::Manual));
    assert_matches!(next_submit_op(&mut source_ops), Op::UserTurn { .. });
    let mut input_state = source
        .capture_thread_input_state()
        .expect("proceed input state");
    input_state.recovered_queue = true;

    let (mut restored, _restored_rx, mut restored_ops) = configured_chat().await;
    restored.restore_reconnected_input(Some(input_state));
    assert_eq!(
        restored
            .pending_handoff_state()
            .and_then(crate::handoff::PendingHandoffState::submitted_text),
        None
    );
    assert!(!restored.maybe_send_next_queued_input());
    assert_no_submit_op(&mut restored_ops);

    restored.submit_user_message(UserMessage::from("continue after reconnect"));
    let submitted = take_user_turn(&mut restored_ops);
    assert_eq!(
        submitted,
        vec![UserInput::Text {
            text: pending.execution_prompt_with_instruction("continue after reconnect"),
            text_elements: Vec::new(),
        }]
    );

    let (mut queued_source, _queued_rx, mut queued_ops) = configured_chat().await;
    assert!(queued_source.submit_handoff_execution(pending, HandoffTrigger::Manual));
    assert_matches!(next_submit_op(&mut queued_ops), Op::UserTurn { .. });
    let mut queued_state = queued_source
        .capture_thread_input_state()
        .expect("queued recovery input state");
    queued_state
        .queued_user_messages
        .push_back(UserMessage::from("unrelated recovered input").into());
    queued_state.recovered_queue = true;
    let (mut queued_restored, _queued_restored_rx, mut queued_restored_ops) =
        configured_chat().await;
    queued_restored.restore_reconnected_input(Some(queued_state));
    let captured = queued_restored
        .capture_thread_input_state()
        .expect("restored queued state");
    assert!(captured.recovered_queue);
    assert_eq!(
        queued_restored.queued_user_message_texts(),
        vec!["unrelated recovered input".to_string()]
    );
    assert!(!queued_restored.maybe_send_next_queued_input());
    assert_no_submit_op(&mut queued_restored_ops);
}

#[tokio::test]
async fn plan_handoff_reconnect_consumes_proceed_committed_before_disconnect() {
    let (mut source, _source_rx, mut source_ops) = configured_chat().await;
    let pending =
        PendingHandoffPlan::new("- Commit exactly once".to_string()).expect("valid handoff plan");
    assert!(source.submit_handoff_execution(pending, HandoffTrigger::Manual));
    assert_matches!(next_submit_op(&mut source_ops), Op::UserTurn { .. });
    let mut input_state = source
        .capture_thread_input_state()
        .expect("proceed input state");
    input_state.recovered_queue = true;

    let (mut restored, mut restored_rx, _restored_ops) = configured_chat().await;
    restored.restore_reconnected_input(Some(input_state));
    assert!(restored.pending_handoff_plan().is_some());
    restored.reconcile_replayed_pending_handoff_submission(/*matched*/ true);

    assert_eq!(restored.pending_handoff_plan(), None);
    assert_matches!(
        restored_rx.try_recv(),
        Ok(AppEvent::PendingHandoffConsumed {
            completion: Some((HandoffTrigger::Manual, HandoffDisposition::Proceed)),
            ..
        })
    );
}

#[tokio::test]
async fn plan_handoff_mode_update_failure_returns_manual_handoff_to_default() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    chat.dispatch_command(SlashCommand::Handoff);
    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
    let requested_mode = chat
        .pending_user_collaboration_mode
        .as_ref()
        .expect("handoff mode update should be pending")
        .mode
        .clone();
    let thread_id = chat.thread_id.expect("configured thread");

    chat.on_collaboration_mode_settings_update_failed(thread_id, &requested_mode);

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert_eq!(chat.collaboration_mode_label(), Some("Default"));
    assert_eq!(chat.active_handoff_generation(), None);
    let history = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(history.contains("Handoff mode could not be applied"));
}

#[tokio::test]
async fn plan_handoff_attachment_only_clarification_owns_its_turn() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    chat.dispatch_command(SlashCommand::Handoff);
    let initial_items = take_user_turn(&mut op_rx);
    commit_user_turn(&mut chat, "initial-plan", initial_items);
    handle_turn_completed(&mut chat, "initial-plan", /*duration_ms*/ None);
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::StartHandoffTransfer { .. }))
    );

    chat.submit_user_message(UserMessage {
        text: String::new(),
        local_images: vec![LocalImageAttachment {
            placeholder: "[Image #1]".to_string(),
            path: test_path_buf("/tmp/handoff-clarification.png"),
        }],
        remote_image_urls: Vec::new(),
        text_elements: Vec::new(),
        mention_bindings: Vec::new(),
    });
    let image_items = take_user_turn(&mut op_rx);
    assert!(matches!(
        image_items.as_slice(),
        [UserInput::LocalImage { .. }]
    ));
    commit_user_turn(&mut chat, "image-clarification", image_items);
    chat.on_plan_item_completed("- Continue with the clarified image".to_string());
    handle_turn_completed(&mut chat, "image-clarification", /*duration_ms*/ None);

    assert!(
        std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::StartHandoffTransfer { plan, .. }
                if plan == "- Continue with the clarified image"
        ))
    );
}

#[tokio::test]
async fn plan_handoff_misalignment_new_chat_requests_pending_confirmation() {
    let (mut chat, mut rx, _op_rx) = configured_chat().await;
    chat.install_pending_handoff(
        PendingHandoffPlan::new("- Preserve this pending plan".to_string())
            .expect("valid pending plan"),
    );
    while rx.try_recv().is_ok() {}
    chat.show_misalignment_policy_precaution();

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::ConfirmNewSessionWithPendingHandoff { name: None })
    );
    assert!(chat.pending_handoff_plan().is_some());

    assert!(chat.confirm_misalignment_new_pending_handoff(/*name*/ None));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_matches!(rx.try_recv(), Ok(AppEvent::RestoreMisalignmentPrecaution));
    chat.show_misalignment_policy_precaution();
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Chat stopped as a precaution"));
}
