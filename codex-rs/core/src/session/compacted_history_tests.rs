use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn compaction_persists_resume_metadata_and_companion_records() {
    let (mut session, turn_context) = make_session_and_context().await;
    let rollout_path = attach_thread_persistence(&mut session).await;
    let turn_context = Arc::new(turn_context);
    turn_context
        .turn_metadata_state
        .set_turn_trigger("automation".to_string());
    turn_context
        .turn_metadata_state
        .set_parent_turn_id("parent-turn".to_string());
    turn_context
        .turn_metadata_state
        .set_root_turn_id("root-turn".to_string());
    turn_context.turn_metadata_state.set_initiating_agent_path(
        codex_protocol::AgentPath::try_from("/root/requester").expect("requester path"),
    );
    let turn_context_baseline = turn_context.to_turn_context_item();
    let world_state = Arc::new(build_world_state_from_turn_context(&session, &turn_context).await);
    let previous_turn_settings = PreviousTurnSettings {
        model: "previous-model".to_string(),
        comp_hash: Some("comp-hash".to_string()),
        cyber_access_program: None,
        realtime_active: Some(true),
    };
    session
        .set_previous_turn_settings(Some(previous_turn_settings.clone()))
        .await;

    session.multi_agent_version = std::sync::OnceLock::from(MultiAgentVersion::V2);
    let expected = CompactionResumeMetadata {
        multi_agent_version: Some(MultiAgentVersion::V2),
        last_started_turn_id: Some(turn_context.sub_id.clone()),
        turn_attribution: Some(codex_history::TurnAttribution {
            turn_id: turn_context.sub_id.clone(),
            turn_trigger: Some("automation".to_string()),
            parent_turn_id: Some("parent-turn".to_string()),
            initiating_agent_path: Some(
                codex_protocol::AgentPath::try_from("/root/requester").expect("requester path"),
            ),
            root_turn_id: turn_context_baseline.root_turn_id.clone(),
        }),
        previous_turn_settings: Some(previous_turn_settings),
    };
    let expected_settings = codex_protocol::protocol::ThreadSettingsAppliedEvent {
        thread_id: Some(session.thread_id()),
        thread_settings: session.thread_settings_snapshot().await,
    };

    session
        .record_started_turn(&turn_context.sub_id, Some(turn_context.attribution()))
        .await;
    let compact_context = session.new_default_turn().await;
    session
        .record_started_turn(&compact_context.sub_id, /*attribution*/ None)
        .await;
    assert_eq!(
        session.state.lock().await.turn_attribution,
        expected.turn_attribution
    );

    session
        .record_started_turn(&turn_context.sub_id, Some(turn_context.attribution()))
        .await;
    let input_goal_ids =
        crate::context::UserGoalUpdate::message_ids(session.clone_history().await.raw_items());
    // The goal edit is accepted after the compaction input was captured.
    session
        .record_user_goal_update(crate::context::UserGoalUpdate::Clear)
        .await
        .expect("record concurrent clear");
    let accepted_goal = session
        .clone_history()
        .await
        .annotated_items()
        .last()
        .cloned()
        .expect("accepted goal");
    let (window_number, window_ids) = session.advance_auto_compact_window().await;
    session
        .replace_compacted_history(
            vec![ResponseItemEnvelope::new(user_message("compacted context"))],
            turn_context_baseline.clone(),
            world_state.render_full().0,
            CompactedHistoryMetadata {
                input_goal_ids,
                message: String::new(),
                window_number,
                window_ids,
                compaction_response_id: None,
                compaction_model_hash: None,
                reviewer_compaction_hash: None,
            },
        )
        .await;
    let live_history = session.clone_history().await.annotated_items().to_vec();
    assert_eq!(&live_history[1..], &[accepted_goal]);

    session.flush_rollout().await.expect("flush checkpoints");
    let (items, _, _) = RolloutRecorder::load_rollout_items(&rollout_path)
        .await
        .expect("read checkpoints");
    let compaction_items = items
        .into_iter()
        .skip_while(|item| !matches!(item, RolloutItem::Compacted(_)))
        .collect::<Vec<_>>();
    let [
        RolloutItem::Compacted(compacted),
        RolloutItem::WorldState(saved_world_state),
        RolloutItem::TurnContext(saved_turn_context),
        RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(saved_settings)),
    ] = compaction_items.as_slice()
    else {
        panic!("unexpected compaction records: {compaction_items:#?}");
    };
    assert_eq!(compacted.replacement_history, Some(live_history));
    assert_eq!(compacted.resume_metadata.as_ref(), Some(&expected));
    assert_eq!(
        saved_world_state,
        &WorldStateItem::full(world_state.render_full().0.into_object())
    );
    assert_eq!(saved_turn_context, &turn_context_baseline);
    assert_eq!(saved_settings, &expected_settings);
    for same_turn_input in [false, true] {
        let mut rollback_items = vec![RolloutItem::Compacted(compacted.clone())];
        if same_turn_input {
            rollback_items.push(RolloutItem::ResponseItem(
                user_message("steered work").into(),
            ));
        }
        rollback_items.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
            codex_protocol::protocol::ThreadRolledBackEvent { num_turns: 1 },
        )));
        let rolled_back = session
            .reconstruct_history_from_rollout(&turn_context, &rollback_items)
            .await;
        assert_eq!(rolled_back.turn_attribution, None);
    }

    // Settings invalidate continuation eligibility; standalone compaction replaces its ID.
    // Neither changes the regular work whose attribution the checkpoint preserves.
    for last_started_turn_id in [None, Some("compact-turn".to_string())] {
        let mut checkpoint = compacted.clone();
        checkpoint
            .resume_metadata
            .as_mut()
            .expect("resume metadata")
            .last_started_turn_id = last_started_turn_id.clone();
        let mut items = vec![RolloutItem::Compacted(checkpoint)];
        if let Some(turn_id) = last_started_turn_id {
            items.push(RolloutItem::EventMsg(EventMsg::TurnComplete(
                TurnCompleteEvent {
                    root_turn_id: None,
                    turn_id,
                    last_agent_message: None,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                    time_to_first_token_ms: None,
                },
            )));
        }
        let reconstructed = session
            .reconstruct_history_from_rollout(&turn_context, &items)
            .await;
        assert_eq!(reconstructed.turn_attribution, expected.turn_attribution);
        assert_eq!(reconstructed.reference_context_item, None);
        items.push(RolloutItem::EventMsg(EventMsg::ThreadRolledBack(
            codex_protocol::protocol::ThreadRolledBackEvent { num_turns: 1 },
        )));
        let rolled_back = session
            .reconstruct_history_from_rollout(&turn_context, &items)
            .await;
        assert_eq!(rolled_back.turn_attribution, None);
    }
}
