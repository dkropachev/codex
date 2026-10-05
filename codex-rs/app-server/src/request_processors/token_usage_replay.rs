//! Replays persisted token usage snapshots when a client attaches to an existing thread.
//!
//! The message processor decides when replay is allowed and preserves JSON-RPC response
//! ordering. This module owns notification construction and the attribution rules that
//! map the latest persisted `TokenCount` back to a v2 turn id.
//!
//! Rollout histories can contain explicit turn ids or generated turn ids. When explicit
//! ids do not match the rebuilt thread, replay falls back to the active turn position at
//! the time the `TokenCount` was persisted so the notification still targets the
//! corresponding rebuilt turn.

use std::collections::HashMap;
use std::sync::Arc;

use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadHistoryBuilder;
use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnStatus;
use codex_core::CodexThread;
use codex_protocol::ThreadId;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::EventMsg;
use codex_rollout::RolloutItem;

use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::thread_state::RestoredTokenUsageAttribution;

/// Sends a restored token usage update to the connection that attached to a thread.
///
/// This is lifecycle replay rather than a model event: the rollout already contains
/// the original `TokenCount`, and emitting through `send_event` here would duplicate
/// persisted usage records. Keeping replay connection-scoped also avoids
/// surprising other subscribers with a historical usage update while they may be
/// rendering live turn events.
pub(super) async fn send_thread_token_usage_update_to_connection(
    outgoing: &Arc<OutgoingMessageSender>,
    connection_id: ConnectionId,
    thread_id: ThreadId,
    conversation: &CodexThread,
    attribution: RestoredTokenUsageAttribution,
) {
    let Some(info) = conversation.token_usage_info().await else {
        return;
    };
    let notification = ThreadTokenUsageUpdatedNotification {
        thread_id: thread_id.to_string(),
        turn_id: attribution.turn_id,
        token_usage: ThreadTokenUsage::from(info),
        usage_after_compaction_item_id: attribution.after_compaction_item_id,
    };
    outgoing
        .send_server_notification_to_connections(
            &[connection_id],
            ServerNotification::ThreadTokenUsageUpdated(notification),
        )
        .await;
}

pub(super) fn restored_token_usage_attribution(
    rollout_items: &[RolloutItem],
    turns: &[Turn],
) -> RestoredTokenUsageAttribution {
    RestoredTokenUsageAttribution {
        turn_id: latest_token_usage_turn_id_from_rollout_items(rollout_items, turns)
            .unwrap_or_else(|| latest_token_usage_turn_id(turns)),
        after_compaction_item_id: latest_usage_after_completed_compaction(rollout_items),
    }
}

fn latest_usage_after_completed_compaction(rollout_items: &[RolloutItem]) -> Option<String> {
    let usage_index = rollout_items
        .iter()
        .rposition(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TokenCount(_))))?;
    let mut started = HashMap::new();
    let mut latest = None;
    for (index, item) in rollout_items.iter().enumerate() {
        match item {
            RolloutItem::EventMsg(EventMsg::ItemStarted(payload))
                if let TurnItem::ContextCompaction(compaction) = &payload.item =>
            {
                started.insert(compaction.id.as_str(), index);
            }
            RolloutItem::EventMsg(EventMsg::ItemCompleted(payload))
                if let TurnItem::ContextCompaction(compaction) = &payload.item
                    && let Some(&start_index) = started.get(compaction.id.as_str())
                    && start_index < usage_index
                    && latest
                        .as_ref()
                        .is_none_or(|(previous_index, _)| start_index > *previous_index) =>
            {
                latest = Some((start_index, compaction.id.clone()));
            }
            _ => {}
        }
    }
    latest.map(|(_, id)| id)
}

/// Identifies the turn that was active when the latest `TokenCount` record appeared.
///
/// The id is preferred when it still appears in the rebuilt thread. The position is a
/// fallback for histories whose implicit turn ids are regenerated during reconstruction.
fn latest_token_usage_turn_id_from_rollout_items(
    rollout_items: &[RolloutItem],
    turns: &[Turn],
) -> Option<String> {
    let token_count_index = rollout_items
        .iter()
        .rposition(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TokenCount(_))))?;
    let mut builder = ThreadHistoryBuilder::new();
    for item in &rollout_items[..token_count_index] {
        builder.handle_rollout_item(item);
    }

    if turns.is_empty() {
        return builder.active_turn_id_if_explicit();
    }

    let active_turn_id = builder.active_turn_id()?;
    if turns.iter().any(|turn| turn.id == active_turn_id) {
        Some(active_turn_id.to_string())
    } else {
        builder
            .active_turn_position()
            .and_then(|position| turns.get(position))
            .map(|turn| turn.id.clone())
    }
}

/// Chooses a fallback turn id that should own a replayed token usage update.
///
/// Normal replay derives the owner from the rollout position of the latest
/// `TokenCount` event. This fallback only preserves a stable wire shape for
/// unusual histories where that rollout information cannot be read.
fn latest_token_usage_turn_id(turns: &[Turn]) -> String {
    turns
        .iter()
        .rev()
        .find(|turn| matches!(turn.status, TurnStatus::Completed | TurnStatus::Failed))
        .or_else(|| turns.last())
        .map(|turn| turn.id.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_app_server_protocol::build_turns_from_rollout_items;
    use codex_protocol::items::ContextCompactionItem;
    use codex_protocol::protocol::AgentMessageEvent;
    use codex_protocol::protocol::ItemCompletedEvent;
    use codex_protocol::protocol::ItemStartedEvent;
    use codex_protocol::protocol::TokenCountEvent;
    use codex_protocol::protocol::UserMessageEvent;
    use pretty_assertions::assert_eq;

    #[test]
    fn replay_attribution_uses_already_loaded_history() {
        let rollout_items = token_usage_history();
        let turns = build_turns_from_rollout_items(&rollout_items);

        assert_eq!(
            latest_token_usage_turn_id_from_rollout_items(&rollout_items, turns.as_slice()),
            Some(turns[0].id.clone())
        );
    }

    #[test]
    fn replay_attribution_falls_back_to_rebuilt_turn_position() {
        let rollout_items = token_usage_history();
        let mut turns = build_turns_from_rollout_items(&rollout_items);
        turns[0].id = "rebuilt-turn-id".to_string();

        assert_eq!(
            latest_token_usage_turn_id_from_rollout_items(&rollout_items, turns.as_slice()),
            Some("rebuilt-turn-id".to_string())
        );
    }

    #[test]
    fn replay_attribution_rejects_suffix_generated_turn_ids() {
        let rollout_items = token_usage_history();

        assert_eq!(
            latest_token_usage_turn_id_from_rollout_items(&rollout_items, /*turns*/ &[]),
            None
        );
    }

    #[test]
    fn replay_attribution_uses_latest_token_count_and_ignores_tail_turn() {
        let mut rollout_items = token_usage_history();
        rollout_items.extend(token_usage_history());
        let turns = build_turns_from_rollout_items(&rollout_items);

        assert_eq!(
            latest_token_usage_turn_id_from_rollout_items(&rollout_items, turns.as_slice()),
            Some(turns[2].id.clone())
        );
    }

    #[test]
    fn replay_usage_correlates_only_a_completed_compaction_started_before_the_count() {
        let thread_id = ThreadId::new();
        let item = TurnItem::ContextCompaction(ContextCompactionItem {
            id: "compact-1".to_string(),
        });
        let started = RolloutItem::EventMsg(EventMsg::ItemStarted(ItemStartedEvent {
            thread_id,
            turn_id: "turn-1".to_string(),
            item: item.clone(),
            started_at_ms: 1,
        }));
        let completed = RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: "turn-1".to_string(),
            item,
            started_at_ms: Some(1),
            completed_at_ms: 2,
        }));
        let count = RolloutItem::EventMsg(EventMsg::TokenCount(TokenCountEvent {
            info: None,
            rate_limits: None,
        }));
        assert_eq!(
            latest_usage_after_completed_compaction(&[
                count.clone(),
                started.clone(),
                completed.clone()
            ]),
            None,
        );
        assert_eq!(
            latest_usage_after_completed_compaction(&[started, count, completed]),
            Some("compact-1".to_string()),
        );
    }

    fn token_usage_history() -> Vec<RolloutItem> {
        vec![
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                client_id: None,
                message: "first turn".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
                ..Default::default()
            })),
            RolloutItem::EventMsg(EventMsg::AgentMessage(AgentMessageEvent {
                message: "first answer".to_string(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            })),
            RolloutItem::EventMsg(EventMsg::TokenCount(TokenCountEvent {
                info: None,
                rate_limits: None,
            })),
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                client_id: None,
                message: "second turn".to_string(),
                images: None,
                local_images: Vec::new(),
                text_elements: Vec::new(),
                ..Default::default()
            })),
        ]
    }
}
