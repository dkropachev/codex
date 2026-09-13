use std::collections::HashSet;
use std::collections::VecDeque;

use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::RolloutItem;

use super::Session;
use crate::context::PendingReviewReport;
use crate::context::ReviewHandoff;
use crate::context_manager::is_user_turn_boundary;

const LEGACY_REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
const LEGACY_REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";

impl Session {
    pub(crate) async fn enqueue_review_report(&self, report: PendingReviewReport) {
        self.state.lock().await.enqueue_review_report(report);
    }

    pub(crate) async fn pending_review_handoff(&self) -> Option<ReviewHandoff> {
        let reports = self.state.lock().await.pending_review_reports();
        ReviewHandoff::new(&reports)
    }

    pub(crate) async fn persist_and_consume_review_handoff(
        &self,
        through_item_id: &str,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().await;
        self.try_persist_rollout_items(&[RolloutItem::ResponseItem(
            ReviewHandoff::consumption_marker(through_item_id),
        )])
        .await?;
        state.clear_pending_review_reports_through(through_item_id);
        Ok(())
    }

    pub(super) async fn restore_pending_review_reports(&self, history: &InitialHistory) {
        let rollout_items: &[RolloutItem] = match history {
            InitialHistory::Resumed(history) => history.history.as_slice(),
            InitialHistory::Forked(history) => history,
            InitialHistory::New | InitialHistory::Cleared => return,
        };
        self.rebuild_pending_review_reports(rollout_items).await;
    }

    pub(crate) async fn rebuild_pending_review_reports(&self, rollout_items: &[RolloutItem]) {
        let mut state = self.state.lock().await;
        state.clear_pending_review_reports();
        for report in pending_review_reports_from_rollout(rollout_items) {
            state.enqueue_review_report(report);
        }
    }
}

fn pending_review_reports_from_rollout(rollout_items: &[RolloutItem]) -> Vec<PendingReviewReport> {
    let mut pending = VecDeque::new();
    let mut seen_item_ids = HashSet::new();
    let mut injected_through = None;
    for item in rollback_surviving_items(rollout_items) {
        match item {
            RolloutItem::EventMsg(EventMsg::ItemCompleted(completed)) => {
                let TurnItem::ExitedReviewMode(exited) = &completed.item else {
                    continue;
                };
                let Some(output) = exited.review_output.as_ref() else {
                    continue;
                };
                if seen_item_ids.insert(exited.id.clone()) {
                    pending.push_back(PendingReviewReport::new(exited.id.clone(), output.clone()));
                }
            }
            RolloutItem::EventMsg(EventMsg::ExitedReviewMode(exited)) => {
                let (Some(item_id), Some(output)) =
                    (exited.item_id.as_ref(), exited.review_output.as_ref())
                else {
                    continue;
                };
                if seen_item_ids.insert(item_id.clone()) {
                    pending.push_back(PendingReviewReport::new(item_id.clone(), output.clone()));
                }
            }
            RolloutItem::ResponseItem(response_item) => {
                if let Some(report) = ReviewHandoff::pending_report(response_item) {
                    if seen_item_ids.insert(report.item_id.clone()) {
                        pending.push_back(report);
                    }
                } else if let Some(item_id) = ReviewHandoff::consumed_through(response_item) {
                    consume_reports_through(&mut pending, item_id);
                    injected_through = None;
                } else if let Some(item_id) = ReviewHandoff::content_through(response_item) {
                    injected_through = Some(item_id.to_string());
                } else if is_user_turn_boundary(response_item)
                    && let Some(item_id) = injected_through.take()
                {
                    consume_reports_through(&mut pending, &item_id);
                } else if is_legacy_review_history_item(response_item) {
                    pending.clear();
                }
            }
            RolloutItem::SessionMeta(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::EventMsg(_) => {}
        }
    }
    pending.into_iter().collect()
}

fn consume_reports_through(pending: &mut VecDeque<PendingReviewReport>, item_id: &str) {
    while let Some(report) = pending.pop_front() {
        if report.item_id == item_id {
            break;
        }
    }
}

fn rollback_surviving_items(rollout_items: &[RolloutItem]) -> Vec<&RolloutItem> {
    let mut surviving: Vec<&RolloutItem> = Vec::new();
    let mut user_positions = Vec::new();
    for item in rollout_items {
        if let RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) = item {
            let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
            let start_position = user_positions
                .len()
                .checked_sub(num_turns)
                .and_then(|index| user_positions.get(index).copied())
                .or_else(|| user_positions.first().copied());
            if let Some(start_position) = start_position {
                surviving.truncate(start_position);
            }
            let retained = user_positions.len().saturating_sub(num_turns);
            user_positions.truncate(retained);
            continue;
        }
        let is_user_boundary = match item {
            RolloutItem::ResponseItem(response_item) => {
                !ReviewHandoff::is_consumption_marker(response_item)
                    && !ReviewHandoff::is_pending_report_marker(response_item)
                    && is_user_turn_boundary(response_item)
            }
            RolloutItem::InterAgentCommunication(_) => true,
            RolloutItem::InterAgentCommunicationMetadata { .. } => true,
            RolloutItem::SessionMeta(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::EventMsg(_) => false,
        };
        if is_user_boundary {
            let mut boundary = surviving.len();
            while boundary > 0
                && matches!(
                    surviving[boundary - 1],
                    RolloutItem::ResponseItem(response_item)
                        if ReviewHandoff::is_content_item(response_item)
                )
            {
                boundary -= 1;
            }
            user_positions.push(boundary);
        }
        surviving.push(item);
    }
    surviving
}

fn is_legacy_review_history_item(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message { id: Some(id), .. }
            if id == LEGACY_REVIEW_USER_MESSAGE_ID || id == LEGACY_REVIEW_ASSISTANT_MESSAGE_ID
    )
}

#[cfg(test)]
#[path = "review_handoff_tests.rs"]
mod tests;
