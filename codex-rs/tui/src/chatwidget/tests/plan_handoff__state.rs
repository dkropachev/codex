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

fn begin_handoff(
    chat: &mut ChatWidget,
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
    args: Option<&str>,
) {
    match args {
        Some(args) => {
            chat.dispatch_command_with_args(SlashCommand::Handoff, args.to_string(), Vec::new())
        }
        None => chat.dispatch_command(SlashCommand::Handoff),
    }
    let items = match next_submit_op(op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected handoff planning turn, got {other:?}"),
    };
    commit_handoff_prompt(chat, "handoff-plan", items);
}

fn commit_handoff_prompt(chat: &mut ChatWidget, turn_id: &str, items: Vec<UserInput>) {
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

fn take_submitted_handoff_prompt(
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
) -> Vec<UserInput> {
    match next_submit_op(op_rx) {
        Op::UserTurn { items, .. } => items,
        other => panic!("expected submitted handoff turn, got {other:?}"),
    }
}

fn complete_handoff_plan(chat: &mut ChatWidget, plan: &str) {
    chat.on_plan_item_completed(plan.to_string());
    handle_turn_completed(chat, "handoff-plan", /*duration_ms*/ None);
}

fn take_transfer(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Option<(String, HandoffDisposition, HandoffTrigger)> {
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::StartHandoffTransfer {
            plan,
            disposition,
            trigger,
            ..
        } = event
        {
            return Some((plan, disposition, trigger));
        }
    }
    None
}

fn contains_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    predicate: impl Fn(&AppEvent) -> bool,
) -> bool {
    while let Ok(event) = rx.try_recv() {
        if predicate(&event) {
            return true;
        }
    }
    false
}

fn advance_automatic_planning_from_event(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let generation = std::iter::from_fn(|| rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::AdvanceAutomaticHandoffPlanning { generation, .. } => Some(generation),
            _ => None,
        })
        .expect("automatic wrap-up should request the app planning gate");
    assert!(chat.continue_automatic_handoff_planning(generation));
}

fn history_text(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> String {
    drain_insert_history(rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n")
}

fn take_transfer_and_history(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> (Option<(String, HandoffDisposition, HandoffTrigger)>, String) {
    let mut transfer = None;
    let mut history = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::StartHandoffTransfer {
                plan,
                disposition,
                trigger,
                ..
            } => transfer = Some((plan, disposition, trigger)),
            AppEvent::InsertHistoryCell(cell) => {
                history.push(lines_to_single_string(&cell.display_lines(/*width*/ 80)));
            }
            _ => {}
        }
    }
    (transfer, history.join("\n"))
}

fn usage_info(total_tokens: i64, last_tokens: i64, context_window: Option<i64>) -> TokenUsageInfo {
    let usage = |total_tokens| TokenUsage {
        total_tokens,
        ..TokenUsage::default()
    };
    TokenUsageInfo {
        total_token_usage: usage(total_tokens),
        last_token_usage: usage(last_tokens),
        model_context_window: context_window,
    }
}

#[derive(Clone, Copy, Debug)]
enum AutomaticBlocker {
    ComposerDraft,
    QueuedMessage,
    PendingSteer,
    PendingStart,
    Modal,
    RequestUserInput,
    RateLimitPrompt,
    RateLimitRecovery,
    ActiveGoal,
    PlanMode,
    HandoffMode,
    SideSession,
    ContextDrop,
}

async fn latched_automatic_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, rx, op_rx) = configured_chat().await;
    chat.config.tui_auto_handoff_threshold_percent = Some(71);
    handle_turn_started(&mut chat, "threshold-turn");
    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 400_000,
            /*last_tokens*/ 74_480,
            Some(100_000),
        )),
    );
    handle_turn_completed(&mut chat, "threshold-turn", /*duration_ms*/ None);
    assert!(chat.automatic_handoff_is_locally_eligible());
    (chat, rx, op_rx)
}

fn install_automatic_blocker(chat: &mut ChatWidget, blocker: AutomaticBlocker) {
    match blocker {
        AutomaticBlocker::ComposerDraft => {
            chat.restore_user_message_to_composer(UserMessage::from("unfinished draft"))
        }
        AutomaticBlocker::QueuedMessage => chat
            .input_queue
            .queued_user_messages
            .push_back(UserMessage::from("queued input").into()),
        AutomaticBlocker::PendingSteer => chat
            .input_queue
            .pending_steers
            .push_back(pending_steer("pending steer")),
        AutomaticBlocker::PendingStart => chat.input_queue.user_turn_pending_start = true,
        AutomaticBlocker::Modal => {
            chat.bottom_pane.show_selection_view(SelectionViewParams {
                title: Some("Blocking modal".to_string()),
                items: vec![SelectionItem {
                    name: "Dismiss".to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        AutomaticBlocker::RequestUserInput => {
            chat.handle_request_user_input_now(ToolRequestUserInputParams {
                thread_id: chat.thread_id.expect("configured thread").to_string(),
                item_id: "request-input".to_string(),
                turn_id: "threshold-turn".to_string(),
                questions: vec![ToolRequestUserInputQuestion {
                    id: "choice".to_string(),
                    header: "Choice".to_string(),
                    question: "Continue?".to_string(),
                    is_other: false,
                    is_secret: false,
                    options: Some(vec![ToolRequestUserInputOption {
                        label: "Continue".to_string(),
                        description: "Continue the current task.".to_string(),
                    }]),
                }],
                is_blocking: true,
                auto_resolution_ms: None,
            });
        }
        AutomaticBlocker::RateLimitPrompt => {
            chat.rate_limit_switch_prompt = RateLimitSwitchPromptState::Pending;
        }
        AutomaticBlocker::RateLimitRecovery => {
            chat.input_queue.rate_limit_recovery_pending = true;
        }
        AutomaticBlocker::ActiveGoal => {
            chat.current_goal_status = Some(GoalStatusState::new(
                AppThreadGoal {
                    thread_id: chat.thread_id.expect("configured thread").to_string(),
                    objective: "Finish the active goal".to_string(),
                    status: AppThreadGoalStatus::Active,
                    token_budget: None,
                    tokens_used: 0,
                    time_used_seconds: 0,
                    created_at: 0,
                    updated_at: 0,
                },
                Instant::now(),
            ));
        }
        AutomaticBlocker::PlanMode => {
            let mask = collaboration_modes::plan_mask(chat.model_catalog.as_ref())
                .expect("plan collaboration mode");
            chat.set_collaboration_mask(mask);
        }
        AutomaticBlocker::HandoffMode => {
            let mask = crate::handoff::handoff_mask(chat.model_catalog.as_ref())
                .expect("handoff collaboration mode");
            chat.set_collaboration_mask(mask);
        }
        AutomaticBlocker::SideSession => chat.set_side_conversation_active(/*active*/ true),
        AutomaticBlocker::ContextDrop => handle_token_count(
            chat,
            Some(usage_info(
                /*total_tokens*/ 400_000,
                /*last_tokens*/ 72_720,
                Some(100_000),
            )),
        ),
    }
}

#[tokio::test]
async fn plan_handoff_completed_default_and_deferred_plans_emit_the_requested_transfer() {
    for (args, disposition) in [
        (None, HandoffDisposition::Proceed),
        (
            Some("--defer keep tests focused"),
            HandoffDisposition::Defer,
        ),
    ] {
        let (mut chat, mut rx, mut op_rx) = configured_chat().await;
        begin_handoff(&mut chat, &mut op_rx, args);

        complete_handoff_plan(&mut chat, "- Continue safely");

        assert_eq!(
            take_transfer(&mut rx),
            Some((
                "- Continue safely".to_string(),
                disposition,
                HandoffTrigger::Manual,
            ))
        );
    }
}

#[tokio::test]
async fn plan_handoff_ask_plan_renders_three_choices_wide_and_narrow_and_escape_stays() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    begin_handoff(&mut chat, &mut op_rx, Some("--ask focus validation"));
    complete_handoff_plan(
        &mut chat,
        "- Run focused validation\n- Continue implementation",
    );

    assert_chatwidget_snapshot!(
        "plan_handoff__ask_popup_wide",
        normalize_snapshot_paths(render_bottom_popup(&chat, /*width*/ 80))
    );
    assert_chatwidget_snapshot!(
        "plan_handoff__ask_popup_narrow",
        normalize_snapshot_paths(render_bottom_popup(&chat, /*width*/ 38))
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert!(contains_event(&mut rx, |event| matches!(
        event,
        AppEvent::StayInHandoff {
            trigger: HandoffTrigger::Manual,
            ..
        }
    )));
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);
    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
}

#[tokio::test]
async fn plan_handoff_ask_choices_emit_proceed_defer_and_explicit_stay() {
    for (down_presses, expected_disposition) in [
        (0, Some(HandoffDisposition::Proceed)),
        (1, Some(HandoffDisposition::Defer)),
        (2, None),
    ] {
        let (mut chat, mut rx, mut op_rx) = configured_chat().await;
        begin_handoff(&mut chat, &mut op_rx, Some("--ask"));
        complete_handoff_plan(&mut chat, "- Continue safely");
        for _ in 0..down_presses {
            chat.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        if let Some(expected_disposition) = expected_disposition {
            assert_eq!(
                take_transfer(&mut rx),
                Some((
                    "- Continue safely".to_string(),
                    expected_disposition,
                    HandoffTrigger::Manual,
                ))
            );
        } else {
            assert!(contains_event(&mut rx, |event| matches!(
                event,
                AppEvent::StayInHandoff {
                    trigger: HandoffTrigger::Manual,
                    ..
                }
            )));
        }
    }
}

#[tokio::test]
async fn plan_handoff_plan_bounds_and_latest_revision_control_transfer() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
    let exact = "é".repeat(crate::handoff::MAX_HANDOFF_PLAN_BYTES / "é".len());
    complete_handoff_plan(&mut chat, &exact);
    assert_eq!(
        take_transfer(&mut rx),
        Some((
            exact.clone(),
            HandoffDisposition::Proceed,
            HandoffTrigger::Manual,
        ))
    );

    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
    let over_limit = format!("{exact}a");
    complete_handoff_plan(&mut chat, &over_limit);
    let (transfer, rendered) = take_transfer_and_history(&mut rx);
    assert_eq!(transfer, None);
    assert!(
        rendered.contains("8193 bytes"),
        "unexpected error: {rendered:?}"
    );
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);

    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
    chat.on_plan_item_completed("- Superseded plan".to_string());
    complete_handoff_plan(&mut chat, "- Latest revised plan");
    assert_eq!(
        take_transfer(&mut rx),
        Some((
            "- Latest revised plan".to_string(),
            HandoffDisposition::Proceed,
            HandoffTrigger::Manual,
        ))
    );
}

#[tokio::test]
async fn plan_handoff_missing_and_empty_plans_leave_source_thread_without_transfer() {
    for plan in [None, Some(" \n\t")] {
        let (mut chat, mut rx, mut op_rx) = configured_chat().await;
        begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
        if let Some(plan) = plan {
            chat.on_plan_item_completed(plan.to_string());
        }
        handle_turn_completed(&mut chat, "handoff-plan", /*duration_ms*/ None);

        assert_eq!(take_transfer(&mut rx), None);
        assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);
        assert_eq!(
            chat.collaboration_mode_label(),
            Some(crate::handoff::HANDOFF_MODE_NAME)
        );
    }

    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
    chat.on_plan_delta("- stale streamed plan".to_string());
    chat.on_plan_item_completed_for_turn(String::new(), Some("handoff-plan".to_string()));
    handle_turn_completed(&mut chat, "handoff-plan", /*duration_ms*/ None);
    assert_eq!(take_transfer(&mut rx), None);
}

#[tokio::test]
async fn plan_handoff_deferred_plan_waits_through_local_and_shell_commands_then_merges_prompt() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    let pending = PendingHandoffPlan::new("- Preserve completed work".to_string())
        .expect("valid pending handoff");
    let expected_prompt = pending.execution_prompt_with_instruction("continue with the next step");
    chat.install_pending_handoff(pending);

    let displayed = history_text(&mut rx);
    assert!(displayed.contains("Pending handoff plan"));
    assert!(displayed.contains("Preserve completed work"));
    let displayed_snapshot = displayed
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    assert_chatwidget_snapshot!("plan_handoff__pending_plan_display", displayed_snapshot);

    chat.dispatch_command(SlashCommand::Status);
    assert!(chat.pending_handoff_plan().is_some());
    chat.submit_user_message(UserMessage::from("!pwd"));
    assert_matches!(
        op_rx.try_recv(),
        Ok(Op::RunUserShellCommand { command }) if command == "pwd"
    );
    assert!(chat.pending_handoff_plan().is_some());
    chat.finalize_turn();

    chat.submit_user_message(UserMessage::from("continue with the next step"));

    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: expected_prompt,
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected merged deferred handoff prompt, got {other:?}"),
    }
    assert!(
        chat.pending_handoff_plan().is_some(),
        "the app must retain the plan until the exact submitted prompt is committed"
    );
}

#[tokio::test]
async fn plan_handoff_deferred_plan_survives_thread_input_state_capture_and_restore() {
    let (mut source, mut source_rx, _source_op_rx) = configured_chat().await;
    source.install_pending_handoff(
        PendingHandoffPlan::new("- Resume the deferred implementation".to_string())
            .expect("valid pending handoff"),
    );
    let _ = history_text(&mut source_rx);
    let input_state = source
        .capture_thread_input_state()
        .expect("thread input state");

    let (mut restored, mut restored_rx, _restored_op_rx) = configured_chat().await;
    restored.restore_thread_input_state(
        Some(input_state),
        ThreadInputStateRestoreMode {
            preserve_in_flight_turn: false,
            redisplay_pending_handoff: true,
        },
    );

    assert_eq!(
        restored
            .pending_handoff_plan()
            .map(PendingHandoffPlan::plan),
        Some("- Resume the deferred implementation")
    );
    let displayed = history_text(&mut restored_rx);
    assert!(displayed.contains("Pending handoff plan"));
    assert!(displayed.contains("Resume the deferred implementation"));
    assert!(source.pending_handoff_plan().is_some());
}

#[tokio::test]
async fn plan_handoff_pending_plan_confirms_clear_and_new_and_warns_before_delete() {
    for (command, name) in [
        (SlashCommand::Clear, None),
        (SlashCommand::Clear, Some("named clear")),
        (SlashCommand::New, None),
        (SlashCommand::New, Some("named new")),
    ] {
        let (mut chat, mut rx, _op_rx) = configured_chat().await;
        chat.install_pending_handoff(
            PendingHandoffPlan::new("- Keep this plan until confirmed".to_string())
                .expect("valid pending handoff"),
        );
        let _ = history_text(&mut rx);

        let dispatch = |chat: &mut ChatWidget| match name {
            Some(name) => {
                chat.dispatch_command_with_args(command, name.to_string(), Vec::new());
            }
            None => chat.dispatch_command(command),
        };
        dispatch(&mut chat);

        assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
        let popup = render_bottom_popup(&chat, /*width*/ 80);
        assert!(popup.contains("Discard the pending handoff?"));
        assert!(popup.contains("Keep the pending handoff"));
        assert!(popup.contains(match command {
            SlashCommand::Clear => "Discard and clear",
            SlashCommand::New => "Discard and start new",
            _ => unreachable!("unexpected destructive command"),
        }));
        if command == SlashCommand::Clear && name.is_none() {
            assert_chatwidget_snapshot!("plan_handoff__pending_discard_confirmation", popup);
        }

        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
        assert!(chat.pending_handoff_plan().is_some());

        dispatch(&mut chat);
        chat.handle_key_event(KeyEvent::from(KeyCode::Down));
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        match (command, name, rx.try_recv()) {
            (SlashCommand::Clear, expected_name, Ok(AppEvent::ClearUi { name })) => {
                assert_eq!(name.as_deref(), expected_name);
            }
            (SlashCommand::New, expected_name, Ok(AppEvent::NewSession { name })) => {
                assert_eq!(name.as_deref(), expected_name);
            }
            (_, _, event) => panic!("expected confirmed destructive event, got {event:?}"),
        }
        assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    let (mut chat, mut rx, _op_rx) = configured_chat().await;
    chat.install_pending_handoff(
        PendingHandoffPlan::new("- Pending deletion warning".to_string())
            .expect("valid pending handoff"),
    );
    let _ = history_text(&mut rx);

    chat.dispatch_command(SlashCommand::Delete);

    assert_matches!(rx.try_recv(), Err(TryRecvError::Empty));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("plan_handoff__pending_delete_confirmation", popup.clone());
    let popup = popup.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(popup.contains("Delete this session?"));
    assert!(
        popup.contains("Deletes this session, subagents, and pending handoff."),
        "unexpected delete confirmation: {popup:?}"
    );
}

#[tokio::test]
async fn plan_handoff_context_hint_uses_active_usage_at_70_percent_and_rearms_after_drop() {
    let (mut chat, mut rx, _op_rx) = configured_chat().await;
    const WINDOW: i64 = 100_000;
    const USED_69: i64 = 72_720;
    const USED_70: i64 = 73_600;

    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 99_000,
            USED_69,
            Some(WINDOW),
        )),
    );
    assert!(!history_text(&mut rx).contains("Context is getting full."));

    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 99_000,
            USED_70,
            Some(WINDOW),
        )),
    );
    let hint = history_text(&mut rx);
    assert!(hint.contains("Context is getting full."));
    assert_chatwidget_snapshot!("plan_handoff__context_hint", hint);

    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 250_000,
            USED_70,
            Some(WINDOW),
        )),
    );
    assert!(!history_text(&mut rx).contains("Context is getting full."));

    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 250_000,
            USED_69,
            Some(WINDOW),
        )),
    );
    let _ = history_text(&mut rx);
    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 250_000,
            USED_70,
            Some(WINDOW),
        )),
    );
    assert!(history_text(&mut rx).contains("Context is getting full."));
}

#[tokio::test]
async fn plan_handoff_unknown_window_has_no_hint_and_replayed_usage_does_not_latch_auto() {
    let (mut chat, mut rx, _op_rx) = configured_chat().await;
    chat.config.tui_auto_handoff_threshold_percent = Some(71);
    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 500_000, /*last_tokens*/ 500_000,
            /*context_window*/ None,
        )),
    );
    assert!(!history_text(&mut rx).contains("Context is getting full."));
    assert!(!chat.automatic_handoff_is_locally_eligible());

    chat.set_token_info(Some(usage_info(
        /*total_tokens*/ 500_000,
        /*last_tokens*/ 74_480,
        /*context_window*/ Some(100_000),
    )));
    chat.observe_handoff_context_usage(/*from_replay*/ true);
    assert!(!chat.automatic_handoff_is_locally_eligible());
}

#[tokio::test]
async fn plan_handoff_live_threshold_completion_starts_wrap_up_then_handoff_planning() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    chat.config.tui_auto_handoff_threshold_percent = Some(71);
    handle_turn_started(&mut chat, "work-turn");
    handle_token_count(
        &mut chat,
        Some(usage_info(
            /*total_tokens*/ 400_000,
            /*last_tokens*/ 74_480,
            Some(100_000),
        )),
    );

    handle_turn_completed(&mut chat, "work-turn", /*duration_ms*/ None);

    let thread_id = chat.thread_id.expect("configured thread");
    assert!(contains_event(&mut rx, |event| matches!(
        event,
        AppEvent::AutomaticHandoffCandidate { thread_id: candidate } if *candidate == thread_id
    )));
    assert!(chat.start_automatic_handoff());
    let wrap_up_items = match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            items,
            collaboration_mode: Some(collaboration_mode),
            ..
        } => {
            assert_eq!(
                items,
                vec![UserInput::Text {
                    text: crate::handoff::AUTOMATIC_WRAP_UP_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }]
            );
            assert_eq!(collaboration_mode.mode, ModeKind::Default);
            items
        }
        other => panic!("expected automatic wrap-up turn, got {other:?}"),
    };

    commit_handoff_prompt(&mut chat, "wrap-up-turn", wrap_up_items);
    handle_turn_completed(&mut chat, "wrap-up-turn", /*duration_ms*/ None);
    advance_automatic_planning_from_event(&mut chat, &mut rx);
    let planning_items = match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            items,
            collaboration_mode: Some(collaboration_mode),
            ..
        } => {
            assert_eq!(
                items,
                vec![UserInput::Text {
                    text: crate::handoff::AUTOMATIC_PLANNING_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }]
            );
            assert_eq!(collaboration_mode.mode, ModeKind::Plan);
            assert_eq!(
                chat.collaboration_mode_label(),
                Some(crate::handoff::HANDOFF_MODE_NAME)
            );
            items
        }
        other => panic!("expected automatic handoff planning turn, got {other:?}"),
    };
    commit_handoff_prompt(&mut chat, "planning-turn", planning_items);
}

#[tokio::test]
async fn plan_handoff_automatic_local_blockers_compaction_and_cancellation_are_single_shot() {
    let blockers = [
        AutomaticBlocker::ComposerDraft,
        AutomaticBlocker::QueuedMessage,
        AutomaticBlocker::PendingSteer,
        AutomaticBlocker::PendingStart,
        AutomaticBlocker::Modal,
        AutomaticBlocker::RequestUserInput,
        AutomaticBlocker::RateLimitPrompt,
        AutomaticBlocker::RateLimitRecovery,
        AutomaticBlocker::ActiveGoal,
        AutomaticBlocker::PlanMode,
        AutomaticBlocker::HandoffMode,
        AutomaticBlocker::SideSession,
        AutomaticBlocker::ContextDrop,
    ];

    for blocker in blockers {
        let (mut chat, _rx, mut op_rx) = latched_automatic_chat().await;
        install_automatic_blocker(&mut chat, blocker);

        assert!(
            !chat.automatic_handoff_is_locally_eligible(),
            "{blocker:?} must block automatic handoff"
        );
        assert!(
            !chat.start_automatic_handoff(),
            "{blocker:?} must not start automatic handoff"
        );
        assert_no_submit_op(&mut op_rx);
    }

    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    assert!(chat.start_automatic_handoff());
    assert!(!chat.start_automatic_handoff());
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: crate::handoff::AUTOMATIC_WRAP_UP_PROMPT.to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected one automatic wrap-up turn, got {other:?}"),
    }
    assert_no_submit_op(&mut op_rx);

    handle_turn_started(&mut chat, "automatic-wrap-up");
    handle_turn_interrupted(&mut chat, "automatic-wrap-up");
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert!(!chat.automatic_handoff_is_locally_eligible());
    chat.qualify_automatic_handoff_after_live_completion();
    assert!(!chat.start_automatic_handoff());
    assert_no_submit_op(&mut op_rx);
    assert!(history_text(&mut rx).contains("Automatic handoff cancelled."));
}

#[tokio::test]
async fn plan_handoff_replay_mode_change_and_navigation_do_not_restart_automatic_handoff() {
    let (mut replayed, _rx, _op_rx) = latched_automatic_chat().await;
    replayed.set_token_info(Some(usage_info(
        /*total_tokens*/ 400_000,
        /*last_tokens*/ 72_720,
        /*context_window*/ Some(100_000),
    )));
    replayed.observe_handoff_context_usage(/*from_replay*/ true);
    replayed.set_token_info(Some(usage_info(
        /*total_tokens*/ 400_000,
        /*last_tokens*/ 74_480,
        Some(100_000),
    )));
    replayed.observe_handoff_context_usage(/*from_replay*/ true);
    replayed.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: replayed.thread_id.expect("configured thread").to_string(),
            turn_id: "historical-turn".to_string(),
            item: ThreadItem::ContextCompaction {
                id: "historical-compaction".to_string(),
            },
            completed_at_ms: 0,
        }),
        Some(ReplayKind::ThreadSnapshot),
    );
    assert!(replayed.automatic_handoff_is_locally_eligible());

    let (mut wrapping, mut wrapping_rx, mut wrapping_ops) = latched_automatic_chat().await;
    assert!(wrapping.start_automatic_handoff());
    let wrapping_items = take_submitted_handoff_prompt(&mut wrapping_ops);
    commit_handoff_prompt(&mut wrapping, "wrap-mode-change", wrapping_items);
    wrapping.set_collaboration_mask_from_user_action(
        collaboration_modes::plan_mask(wrapping.model_catalog.as_ref()).expect("plan mask"),
    );
    handle_turn_completed(&mut wrapping, "wrap-mode-change", /*duration_ms*/ None);
    assert!(!contains_event(&mut wrapping_rx, |event| matches!(
        event,
        AppEvent::AdvanceAutomaticHandoffPlanning { .. }
    )));
    wrapping.qualify_automatic_handoff_after_live_completion();
    assert!(!wrapping.start_automatic_handoff());

    let (mut planning, mut planning_rx, mut planning_ops) = latched_automatic_chat().await;
    assert!(planning.start_automatic_handoff());
    let wrapping_items = take_submitted_handoff_prompt(&mut planning_ops);
    commit_handoff_prompt(
        &mut planning,
        "wrap-before-plan-mode-change",
        wrapping_items,
    );
    handle_turn_completed(
        &mut planning,
        "wrap-before-plan-mode-change",
        /*duration_ms*/ None,
    );
    advance_automatic_planning_from_event(&mut planning, &mut planning_rx);
    let planning_items = take_submitted_handoff_prompt(&mut planning_ops);
    commit_handoff_prompt(&mut planning, "plan-mode-change", planning_items);
    planning.set_collaboration_mask_from_user_action(
        collaboration_modes::default_mode_mask(planning.model_catalog.as_ref())
            .expect("default mask"),
    );
    handle_turn_completed(&mut planning, "plan-mode-change", /*duration_ms*/ None);
    planning.qualify_automatic_handoff_after_live_completion();
    assert!(!planning.start_automatic_handoff());

    let (mut navigating, _rx, mut navigation_ops) = latched_automatic_chat().await;
    assert!(navigating.start_automatic_handoff());
    assert_matches!(next_submit_op(&mut navigation_ops), Op::UserTurn { .. });
    navigating.cancel_automatic_handoff_for_navigation();
    navigating.qualify_automatic_handoff_after_live_completion();
    assert!(!navigating.start_automatic_handoff());
}

#[tokio::test]
async fn plan_handoff_failed_and_interrupted_planning_never_transfer_and_remain_retryable() {
    for status in [
        AppServerTurnStatus::Failed,
        AppServerTurnStatus::Interrupted,
    ] {
        let (mut chat, mut rx, mut op_rx) = configured_chat().await;
        begin_handoff(&mut chat, &mut op_rx, /*args*/ None);
        chat.handle_server_notification(
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: chat.thread_id.expect("configured thread").to_string(),
                turn: app_server_turn(
                    "handoff-plan",
                    status,
                    /*duration_ms*/ None,
                    /*error*/ None,
                ),
            }),
            /*replay_kind*/ None,
        );

        assert_eq!(take_transfer(&mut rx), None);
        assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);
        assert_eq!(
            chat.collaboration_mode_label(),
            Some(crate::handoff::HANDOFF_MODE_NAME)
        );

        chat.submit_user_message(UserMessage::from("Refine the handoff after the failure."));
        let retry_items = take_submitted_handoff_prompt(&mut op_rx);
        commit_handoff_prompt(&mut chat, "retry-plan", retry_items);
        chat.on_plan_item_completed("- Retry with the recovered source thread".to_string());
        handle_turn_completed(&mut chat, "retry-plan", /*duration_ms*/ None);
        assert_eq!(
            take_transfer(&mut rx),
            Some((
                "- Retry with the recovered source thread".to_string(),
                HandoffDisposition::Proceed,
                HandoffTrigger::Manual,
            ))
        );
    }

    let (mut chat, mut rx, mut op_rx) = latched_automatic_chat().await;
    assert!(chat.start_automatic_handoff());
    let wrapping_items = take_submitted_handoff_prompt(&mut op_rx);
    commit_handoff_prompt(&mut chat, "automatic-wrap-up", wrapping_items);
    handle_turn_completed(&mut chat, "automatic-wrap-up", /*duration_ms*/ None);
    advance_automatic_planning_from_event(&mut chat, &mut rx);
    let planning_items = take_submitted_handoff_prompt(&mut op_rx);
    commit_handoff_prompt(&mut chat, "automatic-plan", planning_items);

    handle_turn_interrupted(&mut chat, "automatic-plan");

    assert_eq!(take_transfer(&mut rx), None);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert!(!chat.automatic_handoff_is_locally_eligible());
    assert!(!chat.start_automatic_handoff());
    assert_no_submit_op(&mut op_rx);

    let (mut oversized, mut oversized_rx, mut oversized_ops) = latched_automatic_chat().await;
    assert!(oversized.start_automatic_handoff());
    let wrapping_items = take_submitted_handoff_prompt(&mut oversized_ops);
    commit_handoff_prompt(&mut oversized, "oversized-wrap-up", wrapping_items);
    handle_turn_completed(
        &mut oversized,
        "oversized-wrap-up",
        /*duration_ms*/ None,
    );
    advance_automatic_planning_from_event(&mut oversized, &mut oversized_rx);
    let planning_items = take_submitted_handoff_prompt(&mut oversized_ops);
    commit_handoff_prompt(&mut oversized, "oversized-plan", planning_items);
    oversized.on_plan_item_completed("x".repeat(crate::handoff::MAX_HANDOFF_PLAN_BYTES + 1));
    handle_turn_completed(&mut oversized, "oversized-plan", /*duration_ms*/ None);
    assert_eq!(take_transfer(&mut oversized_rx), None);
    assert_eq!(
        oversized.active_collaboration_mode_kind(),
        ModeKind::Default
    );
    oversized.qualify_automatic_handoff_after_live_completion();
    assert!(!oversized.start_automatic_handoff());
}
