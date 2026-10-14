use crate::compact::CompactedHistoryMetadata;
use crate::compact::SUMMARY_PREFIX;
use crate::compact::build_compacted_history;
use crate::compact::build_compaction_replacement_history;
use crate::context::ContextualUserFragment;
use crate::context::HandoffInventory;
use crate::context::UserGoalUpdate;
use crate::hook_runtime::PostCompactHookOutcome;
use crate::hook_runtime::PreCompactHookOutcome;
use crate::hook_runtime::run_post_compact_hooks;
use crate::hook_runtime::run_pre_compact_hooks;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_analytics::CompactionTrigger;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::AgentStatus;
use std::sync::Arc;

/// Installs a model-provided summary without starting another model request.
pub(crate) async fn install_checkpoint(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    summary: String,
) -> CodexResult<()> {
    let turn_context = &step_context.turn;
    match run_pre_compact_hooks(sess, turn_context, CompactionTrigger::Auto).await {
        PreCompactHookOutcome::Continue => {}
        PreCompactHookOutcome::Stopped => return Err(CodexErr::TurnAborted),
    }

    let item = TurnItem::ContextCompaction(ContextCompactionItem::new());
    sess.emit_turn_item_started(turn_context, &item).await;
    let old_history = sess.clone_history().await;
    let input_goal_ids = UserGoalUpdate::message_ids(old_history.raw_items());
    let summary_text = format!("{SUMMARY_PREFIX}\n{summary}");
    let mut new_history = build_compacted_history(Vec::new(), &[], &summary_text);
    if let Some(item) = new_history.last_mut() {
        item.set_turn_id_if_missing(&turn_context.sub_id);
    }
    let world_state = sess
        .build_world_state_for_step(step_context, /*new_window*/ true)
        .await?;
    let (new_history, world_state_baseline) =
        build_compaction_replacement_history(sess, step_context, &world_state, new_history).await;
    let (window_number, window_ids) = sess.advance_auto_compact_window().await;
    sess.replace_compacted_history(
        new_history,
        step_context.to_turn_context_item(),
        world_state_baseline,
        CompactedHistoryMetadata {
            input_goal_ids,
            message: summary_text,
            window_number,
            window_ids,
            compaction_response_id: None,
            compaction_model_hash: turn_context.model_info().comp_hash.clone(),
            reviewer_compaction_hash: None,
        },
    )
    .await;
    sess.recompute_token_usage(&step_context.turn).await;
    record_live_handles(sess, turn_context).await;
    sess.emit_turn_item_completed(turn_context, item).await;
    if let PostCompactHookOutcome::Stopped =
        run_post_compact_hooks(sess, turn_context, CompactionTrigger::Auto).await
    {
        return Err(CodexErr::TurnAborted);
    }
    Ok(())
}

async fn record_live_handles(sess: &Session, turn: &TurnContext) {
    let mut entries = Vec::new();
    if let Ok(agents) = sess
        .services
        .agent_control
        .list(
            sess.thread_id,
            turn.parent_thread_id,
            &turn.session_source,
            /*path_prefix*/ None,
        )
        .await
    {
        for agent in agents
            .into_iter()
            .filter(|agent| agent.thread_id != sess.thread_id)
            .take(/*n*/ 32)
        {
            let name = agent
                .metadata
                .agent_path
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| agent.thread_id.to_string());
            let status = match agent.status {
                AgentStatus::PendingInit => "pending_init",
                AgentStatus::Running => "running",
                AgentStatus::Interrupted => "interrupted",
                AgentStatus::Completed(_) => "completed",
                AgentStatus::Errored(_) => "errored",
                AgentStatus::Shutdown => "shutdown",
                AgentStatus::NotFound => "not_found",
            };
            entries.push(format!("agent {} ({name}): {status}", agent.thread_id));
        }
    }
    for process in sess
        .services
        .unified_exec_manager
        .list_processes()
        .await
        .into_iter()
        .take(/*n*/ 32)
    {
        entries.push(format!("process {}", process.process_id));
    }
    if entries.is_empty() {
        return;
    }
    let inventory = entries.join("\n");
    let item = ContextualUserFragment::into(HandoffInventory(inventory));
    sess.record_conversation_items(turn, turn.model_info(), std::slice::from_ref(&item))
        .await;
}
