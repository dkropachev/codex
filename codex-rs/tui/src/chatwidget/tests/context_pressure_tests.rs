use super::*;
use pretty_assertions::assert_eq;

fn history_text(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> String {
    let lines = drain_insert_history(rx)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    lines_to_single_string(&lines)
}

fn context_usage(total_tokens: i64) -> TokenUsageInfo {
    make_token_info(total_tokens, /*context_window*/ 13_000)
}

fn usage_update(
    thread_id: ThreadId,
    turn_id: &str,
    total_tokens: i64,
    after_compaction_item_id: Option<&str>,
) -> ServerNotification {
    let usage = codex_app_server_protocol::TokenUsageBreakdown {
        total_tokens,
        input_tokens: total_tokens,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 0,
        reasoning_output_tokens: 0,
    };
    ServerNotification::ThreadTokenUsageUpdated(
        codex_app_server_protocol::ThreadTokenUsageUpdatedNotification {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            token_usage: codex_app_server_protocol::ThreadTokenUsage {
                total: usage.clone(),
                last: usage,
                model_context_window: Some(13_000),
            },
            usage_after_compaction_item_id: after_compaction_item_id.map(str::to_string),
        },
    )
}

fn compaction_completed(id: &str) -> ServerNotification {
    compaction_completed_on_turn(id, "turn-1")
}

fn compaction_started_on_turn(id: &str, turn_id: &str) -> ServerNotification {
    ServerNotification::ItemStarted(ItemStartedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: turn_id.to_string(),
        started_at_ms: chrono::Utc::now().timestamp_millis(),
        item: AppServerThreadItem::ContextCompaction { id: id.to_string() },
    })
}

fn compaction_completed_on_turn(id: &str, turn_id: &str) -> ServerNotification {
    ServerNotification::ItemCompleted(ItemCompletedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: turn_id.to_string(),
        completed_at_ms: 0,
        item: AppServerThreadItem::ContextCompaction { id: id.to_string() },
    })
}

fn compacted_turn(turn_id: &str, id: &str) -> AppServerTurn {
    let mut turn = app_server_turn(
        turn_id,
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    turn.items = vec![AppServerThreadItem::ContextCompaction { id: id.to_string() }];
    turn
}

#[tokio::test]
async fn context_pressure_hint_starts_at_seventy_percent_of_adjusted_active_usage() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let mut info = context_usage(/*total_tokens*/ 12_690);
    info.total_token_usage.total_tokens = 100_000;
    chat.set_token_info(Some(info.clone()));
    assert_eq!(history_text(&mut rx), "");
    chat.handle_server_notification(compaction_completed("compact-1"), /*replay_kind*/ None);
    history_text(&mut rx);

    info.last_token_usage.total_tokens = 12_700;
    chat.set_token_info(Some(info.clone()));
    assert_chatwidget_snapshot!(
        "context_pressure_seventy_percent_hint",
        history_text(&mut rx)
    );
    chat.set_token_info(Some(info.clone()));
    assert_eq!(history_text(&mut rx), "");
    chat.thread_id = Some(ThreadId::new());
    info.model_context_window = None;
    chat.set_token_info(Some(info));
    assert_eq!(history_text(&mut rx), "");
}

#[tokio::test]
async fn replayed_compaction_id_does_not_rearm_but_new_background_item_does() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    chat.handle_server_notification(compaction_completed("compact-1"), /*replay_kind*/ None);
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    resumed.replay_thread_turns(
        vec![compacted_turn("turn-1", "compact-1")],
        ReplayKind::ResumeInitialMessages,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");

    resumed.handle_server_notification(
        compaction_completed("compact-2"),
        Some(ReplayKind::ThreadSnapshot),
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut resumed_rx).contains("Context use has reached 70%."));

    let (mut fresh, mut fresh_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    fresh.thread_id = Some(ThreadId::new());
    fresh.inherit_context_pressure_state(&resumed);
    fresh.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut fresh_rx).contains("Context use has reached 70%."));
}

#[tokio::test]
async fn only_completed_compaction_rearms_after_usage_falls_during_compaction() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    history_text(&mut rx);

    chat.handle_server_notification(
        compaction_started_on_turn("aborted", "turn-1"),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        usage_update(thread_id, "turn-1", /*total_tokens*/ 12_699, None),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        usage_update(thread_id, "turn-1", /*total_tokens*/ 12_700, None),
        /*replay_kind*/ None,
    );
    assert_eq!(
        history_text(&mut rx)
            .matches("Context use has reached 70%.")
            .count(),
        0
    );

    chat.handle_server_notification(
        compaction_started_on_turn("completed", "turn-2"),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        usage_update(thread_id, "turn-2", /*total_tokens*/ 12_699, None),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        compaction_started_on_turn("completed", "turn-2"),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        compaction_completed_on_turn("completed", "turn-2"),
        /*replay_kind*/ None,
    );
    history_text(&mut rx);
    chat.handle_server_notification(
        usage_update(thread_id, "turn-2", /*total_tokens*/ 12_700, None),
        /*replay_kind*/ None,
    );
    assert!(history_text(&mut rx).contains("Context use has reached 70%."));
}

#[tokio::test]
async fn first_compaction_seen_during_navigation_rearms_the_hint() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let hint_turn_id = "01900000-0000-7000-8000-000000000001";
    let compact_turn_id = "01900000-0001-7000-8000-000000000001";
    chat.thread_id = Some(thread_id);
    chat.replay_thread_turns(
        vec![app_server_turn(
            hint_turn_id,
            AppServerTurnStatus::Completed,
            /*duration_ms*/ None,
            /*error*/ None,
        )],
        ReplayKind::ResumeInitialMessages,
    );
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    resumed.set_token_info(/*info*/ None);
    resumed.replay_thread_turns(
        vec![compacted_turn(compact_turn_id, "compact-1")],
        ReplayKind::ResumeInitialMessages,
    );
    history_text(&mut resumed_rx);
    resumed.turn_lifecycle.last_turn_id = Some(compact_turn_id.to_string());
    resumed.handle_server_notification(
        usage_update(
            thread_id,
            compact_turn_id,
            /*total_tokens*/ 12_699,
            /*after_compaction_item_id*/ None,
        ),
        /*replay_kind*/ None,
    );
    handle_token_count(&mut resumed, Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");
    resumed.set_token_info(/*info*/ None);
    resumed.handle_server_notification(
        usage_update(
            thread_id,
            compact_turn_id,
            /*total_tokens*/ 12_699,
            Some("compact-1"),
        ),
        /*replay_kind*/ None,
    );
    handle_token_count(&mut resumed, Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut resumed_rx).contains("Context use has reached 70%."));
}

#[tokio::test]
async fn bounded_replay_distinguishes_newer_compaction_from_older_page() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let previous_turn_id = "01900000-0000-7000-8000-000000000001";
    let newer_turn_id = "01900000-0001-7000-8000-000000000001";
    let hint_turn_id = "018f0000-0000-7000-8000-000000000001";
    let regenerated_older_turn_id = "01900000-0002-7000-8000-000000000001";
    chat.thread_id = Some(thread_id);
    chat.turn_lifecycle.last_turn_id = Some(hint_turn_id.to_string());
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    chat.handle_server_notification(
        compaction_completed_on_turn("item-1", previous_turn_id),
        /*replay_kind*/ None,
    );

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    resumed.replay_thread_turns(
        vec![compacted_turn(newer_turn_id, "item-2")],
        ReplayKind::ResumeInitialMessages,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_800)));
    assert_eq!(history_text(&mut resumed_rx), "");
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut resumed_rx).contains("Context use has reached 70%."));

    resumed.replay_thread_turns(
        vec![compacted_turn(regenerated_older_turn_id, "item-0")],
        ReplayKind::ThreadSnapshot,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");

    resumed.handle_server_notification(
        compaction_completed_on_turn("item-1", previous_turn_id),
        Some(ReplayKind::ThreadSnapshot),
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");
}

#[tokio::test]
async fn cached_low_usage_before_compaction_does_not_rearm() {
    let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let turn_id = "rollout-2";
    let older_turn_id = "rollout-1";
    chat.thread_id = Some(thread_id);
    chat.turn_lifecycle.last_turn_id = Some(turn_id.to_string());
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    chat.handle_server_notification(
        compaction_completed_on_turn("compact-1", turn_id),
        /*replay_kind*/ None,
    );
    history_text(&mut rx);
    chat.turn_lifecycle.last_turn_id = Some(older_turn_id.to_string());
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_699)));
    assert_eq!(history_text(&mut rx), "");
    chat.turn_lifecycle.last_turn_id = Some(turn_id.to_string());
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut rx), "");
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_699)));
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut rx).contains("Context use has reached 70%."));
    chat.handle_server_notification(
        compaction_completed_on_turn("compact-1", turn_id),
        /*replay_kind*/ None,
    );
    history_text(&mut rx);
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_699)));
    handle_token_count(&mut chat, Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut rx), "");
}

#[tokio::test]
async fn older_replay_page_without_previous_compaction_does_not_rearm() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let earlier_turn_id = "01900000-0000-7000-8000-000000000001";
    let hint_turn_id = "01900000-0001-7000-8000-000000000001";
    let new_turn_id = "01900000-0002-7000-8000-000000000001";
    chat.thread_id = Some(thread_id);
    chat.record_context_pressure_item("item-10", earlier_turn_id);
    let mut unloaded = app_server_turn(
        hint_turn_id,
        AppServerTurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    unloaded.items_view = codex_app_server_protocol::TurnItemsView::NotLoaded;
    chat.replay_thread_turns(vec![unloaded], ReplayKind::ResumeInitialMessages);
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    resumed.replay_thread_turns(
        vec![compacted_turn(hint_turn_id, "item-11")],
        ReplayKind::ThreadSnapshot,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");

    resumed.replay_thread_turns(
        vec![compacted_turn(new_turn_id, "item-12")],
        ReplayKind::ThreadSnapshot,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert!(history_text(&mut resumed_rx).contains("Context use has reached 70%."));
}

#[tokio::test]
async fn compaction_before_last_hint_does_not_rearm_from_older_page() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let first_turn_id = "01900000-0000-7000-8000-000000000001";
    let second_turn_id = "01900000-0001-7000-8000-000000000001";
    let hint_turn_id = "01900000-0002-7000-8000-000000000001";
    chat.thread_id = Some(thread_id);
    chat.handle_server_notification(
        compaction_completed_on_turn("compact-1", first_turn_id),
        /*replay_kind*/ None,
    );
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    chat.turn_lifecycle.last_turn_id = Some(hint_turn_id.to_string());
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    resumed.replay_thread_turns(
        vec![compacted_turn(second_turn_id, "compact-2")],
        ReplayKind::ThreadSnapshot,
    );
    history_text(&mut resumed_rx);
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
    resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
    assert_eq!(history_text(&mut resumed_rx), "");
}

#[tokio::test]
async fn same_turn_replay_uses_item_cursor_to_reject_old_and_accept_new_compaction() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    let earlier_turn_id = "01900000-0000-7000-8000-000000000001";
    let turn_id = "01900000-0001-7000-8000-000000000001";
    let regenerated_old_turn_id = "01900000-0002-7000-8000-000000000001";
    let new_compaction_id = "01900000-0003-7000-8000-000000000001";
    chat.thread_id = Some(thread_id);
    chat.handle_server_notification(
        compaction_completed_on_turn("item-1", earlier_turn_id),
        /*replay_kind*/ None,
    );
    chat.turn_lifecycle.last_turn_id = Some(turn_id.to_string());
    chat.record_context_pressure_item("tool-call-10", turn_id);
    chat.turn_lifecycle.agent_turn_running = true;
    chat.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));

    let (mut resumed, mut resumed_rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    resumed.thread_id = Some(thread_id);
    resumed.inherit_context_pressure_state(&chat);
    for (id, replay_turn_id, should_hint) in [
        (new_compaction_id, turn_id, true),
        ("item-5", regenerated_old_turn_id, false),
    ] {
        resumed.replay_thread_turns(
            vec![compacted_turn(replay_turn_id, id)],
            ReplayKind::ThreadSnapshot,
        );
        history_text(&mut resumed_rx);
        resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_699)));
        resumed.set_token_info(Some(context_usage(/*total_tokens*/ 12_700)));
        assert_eq!(
            history_text(&mut resumed_rx).contains("Context use has reached 70%."),
            should_hint,
        );
    }
}
