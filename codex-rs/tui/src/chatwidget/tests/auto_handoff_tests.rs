use super::*;

#[tokio::test]
async fn automatic_handoff_requires_opt_in_and_live_context_usage() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.turn_lifecycle.agent_turn_running = true;
    let usage = make_token_info(/*total_tokens*/ 12_800, /*context_window*/ 13_000);

    chat.observe_automatic_handoff_usage(
        &usage,
        context_pressure::UsageUpdate::LiveServerTurn("turn-1"),
    );
    chat.request_automatic_handoff_check();
    assert!(events.try_recv().is_err());

    chat.config.tui_auto_handoff_threshold_percent = Some(80);
    chat.observe_automatic_handoff_usage(&usage, context_pressure::UsageUpdate::Uncorrelated);
    chat.request_automatic_handoff_check();
    assert!(events.try_recv().is_err());

    chat.observe_automatic_handoff_usage(
        &usage,
        context_pressure::UsageUpdate::LiveServerTurn("turn-1"),
    );
    chat.request_automatic_handoff_check();
    assert!(matches!(
        events.try_recv(),
        Ok(AppEvent::AutomaticHandoffCandidate { thread_id: candidate }) if candidate == thread_id
    ));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn automatic_handoff_candidate_clears_after_live_context_drop() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.config.tui_auto_handoff_threshold_percent = Some(80);
    chat.turn_lifecycle.agent_turn_running = true;
    let high = make_token_info(/*total_tokens*/ 12_800, /*context_window*/ 13_000);
    let low = make_token_info(/*total_tokens*/ 12_700, /*context_window*/ 13_000);

    chat.observe_automatic_handoff_usage(
        &high,
        context_pressure::UsageUpdate::LiveServerTurn("turn-1"),
    );
    chat.observe_automatic_handoff_usage(
        &low,
        context_pressure::UsageUpdate::LiveServerTurn("turn-1"),
    );
    chat.request_automatic_handoff_check();

    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn automatic_wrap_up_completion_requests_planning_gate() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    let thread_id = ThreadId::new();
    chat.thread_id = Some(thread_id);
    chat.config.tui_auto_handoff_threshold_percent = Some(80);
    let usage = make_token_info(/*total_tokens*/ 12_800, /*context_window*/ 13_000);
    chat.set_token_info(Some(usage.clone()));
    chat.turn_lifecycle.agent_turn_running = true;
    chat.observe_automatic_handoff_usage(
        &usage,
        context_pressure::UsageUpdate::LiveServerTurn("source-turn"),
    );
    chat.turn_lifecycle.agent_turn_running = false;

    chat.start_automatic_handoff();
    let wrap_up_lines = drain_insert_history(&mut events)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_chatwidget_snapshot!(
        "automatic_handoff_wrap_up_prompt",
        lines_to_single_string(&wrap_up_lines)
    );
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected an automatic wrap-up turn");
    };
    chat.bind_handoff_turn_start("wrap-up-turn", &items);
    chat.note_handoff_turn_completed("wrap-up-turn");
    chat.on_task_complete(
        /*last_agent_message*/ None, /*completion*/ None, /*from_replay*/ false,
    );

    let generation = std::iter::from_fn(|| events.try_recv().ok()).find_map(|event| match event {
        AppEvent::AdvanceAutomaticHandoffPlanning {
            source_thread_id,
            generation,
        } if source_thread_id == thread_id => Some(generation),
        _ => None,
    });
    let generation = generation.expect("automatic planning gate event");
    chat.continue_automatic_handoff_planning(generation);
    let planning_lines = drain_insert_history(&mut events)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    assert_chatwidget_snapshot!(
        "automatic_handoff_planning_prompt",
        lines_to_single_string(&planning_lines)
    );
    assert!(matches!(next_submit_op(&mut ops), Op::UserTurn { .. }));
}

#[tokio::test]
async fn compaction_below_threshold_cancels_automatic_wrap_up() {
    let (mut chat, mut events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.config.tui_auto_handoff_threshold_percent = Some(80);
    let high = make_token_info(/*total_tokens*/ 12_800, /*context_window*/ 13_000);
    let low = make_token_info(/*total_tokens*/ 12_700, /*context_window*/ 13_000);
    chat.set_token_info(Some(high.clone()));
    chat.turn_lifecycle.agent_turn_running = true;
    chat.observe_automatic_handoff_usage(
        &high,
        context_pressure::UsageUpdate::LiveServerTurn("source-turn"),
    );
    chat.turn_lifecycle.agent_turn_running = false;
    chat.start_automatic_handoff();
    let Op::UserTurn { .. } = next_submit_op(&mut ops) else {
        panic!("expected an automatic wrap-up turn");
    };
    chat.note_automatic_handoff_compaction(context_pressure::CompactionObservation::Live);
    chat.observe_automatic_handoff_usage(
        &low,
        context_pressure::UsageUpdate::LiveServerTurn("wrap-up-turn"),
    );
    chat.on_task_complete(
        /*last_agent_message*/ None, /*completion*/ None, /*from_replay*/ false,
    );

    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::AdvanceAutomaticHandoffPlanning { .. }))
    );
}
