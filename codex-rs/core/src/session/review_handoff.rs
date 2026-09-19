use std::collections::HashSet;
use std::collections::VecDeque;

use codex_protocol::items::TurnItem;
use codex_protocol::items::UserMessageItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::RolloutItem;

use super::Session;
use super::turn_context::TurnContext;
use crate::context::PendingReviewReport;
use crate::context::ReviewHandoff;
use crate::context_manager::is_user_turn_boundary;

const LEGACY_REVIEW_USER_MESSAGE_ID: &str = "review_rollout_user";
const LEGACY_REVIEW_ASSISTANT_MESSAGE_ID: &str = "review_rollout_assistant";

pub(crate) enum ReviewHandoffLeadingInput {
    ResponseItem(ResponseItem),
    InterAgentCommunication(InterAgentCommunication),
}

impl Session {
    pub(crate) async fn enqueue_review_report(&self, report: PendingReviewReport) {
        self.state.lock().await.enqueue_review_report(report);
    }

    pub(crate) async fn pending_review_handoff(&self) -> Option<ReviewHandoff> {
        let state = self.state.lock().await;
        let reports = state.pending_review_reports();
        ReviewHandoff::new_with_overflow(&reports, state.pending_review_report_overflow())
    }

    pub(crate) async fn record_review_handoff_and_user_prompt(
        &self,
        turn_context: &TurnContext,
        leading_input: Vec<ReviewHandoffLeadingInput>,
        mut handoff_items: Vec<ResponseItem>,
        transaction_id: &str,
        through_item_id: &str,
        input: &[codex_protocol::user_input::UserInput],
        client_id: Option<String>,
    ) -> anyhow::Result<()> {
        let mut raw_items = Vec::with_capacity(leading_input.len() + handoff_items.len() + 1);
        let mut communication_metadata = Vec::new();
        for leading in leading_input {
            match leading {
                ReviewHandoffLeadingInput::ResponseItem(item) => raw_items.push(item),
                ReviewHandoffLeadingInput::InterAgentCommunication(mut communication) => {
                    communication.set_turn_id_if_missing(&turn_context.sub_id);
                    communication_metadata.push((raw_items.len(), communication.trigger_turn));
                    raw_items.push(communication.to_model_input_item());
                }
            }
        }
        raw_items.append(&mut handoff_items);
        raw_items.push(self.response_item_from_user_input(input.to_vec()));
        let items = self
            .prepare_conversation_items_for_history(turn_context, &raw_items)
            .into_owned();
        let mut rollout_items = Vec::with_capacity(items.len() + communication_metadata.len() + 2);
        rollout_items.push(RolloutItem::ResponseItem(
            ReviewHandoff::transaction_marker(transaction_id),
        ));
        let communication_metadata = communication_metadata
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        for (index, item) in items.iter().cloned().enumerate() {
            if let Some(trigger_turn) = communication_metadata.get(&index) {
                rollout_items.push(RolloutItem::InterAgentCommunicationMetadata {
                    trigger_turn: *trigger_turn,
                });
            }
            rollout_items.push(RolloutItem::ResponseItem(item));
        }
        rollout_items.push(RolloutItem::ResponseItem(
            ReviewHandoff::committed_transaction_marker(transaction_id, through_item_id),
        ));
        if let Err(error) = self.try_persist_rollout_items(&rollout_items).await {
            // The rollout writer retains an unwritten suffix for a later flush. The
            // begin/commit markers make a partial or repeated batch safe on replay.
            tracing::warn!(%error, "review handoff batch was queued for persistence retry");
        }
        {
            let mut state = self.state.lock().await;
            state.current_time_reminder.note_recorded_items(&items);
            state.record_items(
                items.iter(),
                turn_context.model_info.truncation_policy.into(),
            );
            state.clear_pending_review_reports_through(through_item_id);
        }
        self.send_raw_response_items(turn_context, &items).await;

        let mut user_message_item = UserMessageItem::new(input);
        user_message_item.client_id = client_id;
        let turn_item = TurnItem::UserMessage(user_message_item);
        self.emit_turn_item_started(turn_context, &turn_item).await;
        self.emit_turn_item_completed(turn_context, turn_item).await;
        self.ensure_rollout_materialized().await;
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
    let mut transaction_through = None;
    let mut legacy_review_report_already_visible = false;
    for item in rollback_surviving_items(rollout_items) {
        if !matches!(
            item,
            RolloutItem::ResponseItem(_) | RolloutItem::InterAgentCommunicationMetadata { .. }
        ) {
            transaction_through = None;
            injected_through = None;
        }
        match item {
            RolloutItem::EventMsg(EventMsg::ItemCompleted(completed)) => {
                if matches!(&completed.item, TurnItem::EnteredReviewMode(_)) {
                    legacy_review_report_already_visible = false;
                    continue;
                }
                let TurnItem::ExitedReviewMode(exited) = &completed.item else {
                    continue;
                };
                let report_already_visible =
                    std::mem::take(&mut legacy_review_report_already_visible);
                let Some(output) = exited.review_output.as_ref() else {
                    continue;
                };
                if report_already_visible {
                    seen_item_ids.insert(exited.id.clone());
                    continue;
                }
                if seen_item_ids.insert(exited.id.clone()) {
                    pending.push_back(PendingReviewReport::new(exited.id.clone(), output.clone()));
                }
            }
            RolloutItem::EventMsg(EventMsg::ExitedReviewMode(exited)) => {
                let report_already_visible =
                    std::mem::take(&mut legacy_review_report_already_visible);
                let (Some(item_id), Some(output)) =
                    (exited.item_id.as_ref(), exited.review_output.as_ref())
                else {
                    continue;
                };
                if report_already_visible {
                    seen_item_ids.insert(item_id.clone());
                    continue;
                }
                if seen_item_ids.insert(item_id.clone()) {
                    pending.push_back(PendingReviewReport::new(item_id.clone(), output.clone()));
                }
            }
            RolloutItem::EventMsg(EventMsg::EnteredReviewMode(_)) => {
                legacy_review_report_already_visible = false;
            }
            RolloutItem::ResponseItem(response_item) => {
                if is_legacy_review_user_message(response_item) {
                    legacy_review_report_already_visible = true;
                } else if matches!(
                    response_item,
                    ResponseItem::Message { id: Some(id), .. }
                        if id == LEGACY_REVIEW_ASSISTANT_MESSAGE_ID
                ) {
                    legacy_review_report_already_visible = false;
                }
                if let Some(item_id) = ReviewHandoff::transaction_through(response_item) {
                    transaction_through = Some(item_id.to_string());
                    injected_through = None;
                } else if let Some(report) = ReviewHandoff::pending_report(response_item) {
                    if seen_item_ids.insert(report.item_id.clone()) {
                        pending.push_back(report);
                    }
                } else if let Some((transaction_id, item_id)) =
                    ReviewHandoff::committed_transaction(response_item)
                {
                    consume_reports_through(&mut pending, item_id);
                    injected_through = None;
                    if transaction_through.as_deref() == Some(transaction_id) {
                        transaction_through = None;
                    }
                } else if let Some(item_id) = ReviewHandoff::consumed_through(response_item) {
                    consume_reports_through(&mut pending, item_id);
                    injected_through = None;
                } else if let Some(item_id) = ReviewHandoff::content_through(response_item) {
                    if transaction_through.as_deref() != Some(item_id) {
                        injected_through = Some(item_id.to_string());
                    }
                } else if is_user_turn_boundary(response_item)
                    && transaction_through.is_none()
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

pub(super) struct ReviewHandoffHistoryFilter {
    pending_through: Option<String>,
    pending_items: Vec<ResponseItem>,
    transaction_requires_commit: bool,
    transaction_invalid: bool,
    next_fragment_index: usize,
    handoff_bytes: usize,
    committed_transactions: HashSet<String>,
    saw_handoff: bool,
    saw_user_boundary: bool,
}

impl ReviewHandoffHistoryFilter {
    pub(super) fn new() -> Self {
        Self {
            pending_through: None,
            pending_items: Vec::new(),
            transaction_requires_commit: false,
            transaction_invalid: false,
            next_fragment_index: 0,
            handoff_bytes: 0,
            committed_transactions: HashSet::new(),
            saw_handoff: false,
            saw_user_boundary: false,
        }
    }

    pub(super) fn push(&mut self, item: ResponseItem) -> Vec<ResponseItem> {
        if let Some(through) = ReviewHandoff::transaction_through(&item) {
            self.interrupt();
            self.pending_through = Some(through.to_string());
            self.transaction_requires_commit = true;
            return Vec::new();
        }
        if ReviewHandoff::is_pending_report_marker(&item) {
            return Vec::new();
        }
        if let Some((transaction_id, _through)) = ReviewHandoff::committed_transaction(&item) {
            if self.transaction_requires_commit {
                let valid = self.pending_through.as_deref() == Some(transaction_id)
                    && !self.transaction_invalid
                    && self.saw_handoff
                    && self.saw_user_boundary
                    && self
                        .committed_transactions
                        .insert(transaction_id.to_string());
                let completed = valid.then(|| std::mem::take(&mut self.pending_items));
                self.interrupt();
                return completed.unwrap_or_default();
            }
            return Vec::new();
        }
        if ReviewHandoff::consumed_through(&item).is_some() {
            return Vec::new();
        }
        if self.transaction_requires_commit {
            self.validate_transaction_item(&item);
            if !self.transaction_invalid {
                self.pending_items.push(item);
            }
            return Vec::new();
        }
        if let Some((through, index)) = ReviewHandoff::content_part(&item) {
            if self.pending_through.as_deref() != Some(through) {
                self.interrupt();
                self.pending_through = Some(through.to_string());
            }
            if index != self.next_fragment_index
                || !ReviewHandoff::valid_content_item(&item)
                || self.next_fragment_index == crate::context::MAX_HANDOFF_FRAGMENTS
            {
                self.interrupt();
                return Vec::new();
            }
            let content_bytes = match &item {
                ResponseItem::Message { content, .. } => content
                    .iter()
                    .map(|content| match content {
                        codex_protocol::models::ContentItem::InputText { text } => text.len(),
                        _ => usize::MAX,
                    })
                    .sum(),
                _ => usize::MAX,
            };
            self.handoff_bytes = self.handoff_bytes.saturating_add(content_bytes);
            self.next_fragment_index += 1;
            if self.handoff_bytes
                > crate::context::MAX_HANDOFF_LOGICAL_BYTES
                    + crate::context::MAX_HANDOFF_FRAGMENTS * 64
            {
                self.interrupt();
                return Vec::new();
            }
            self.pending_items.push(item);
            return Vec::new();
        }
        if is_user_turn_boundary(&item) && !self.pending_items.is_empty() {
            let mut completed = std::mem::take(&mut self.pending_items);
            self.pending_through = None;
            completed.push(item);
            return completed;
        }
        self.interrupt();
        vec![item]
    }

    fn validate_transaction_item(&mut self, item: &ResponseItem) {
        if let Some((through, index)) = ReviewHandoff::content_part(item) {
            self.saw_handoff = true;
            let content_bytes = match item {
                ResponseItem::Message { content, .. } => content
                    .iter()
                    .map(|content| match content {
                        codex_protocol::models::ContentItem::InputText { text } => text.len(),
                        _ => usize::MAX,
                    })
                    .sum(),
                _ => usize::MAX,
            };
            self.handoff_bytes = self.handoff_bytes.saturating_add(content_bytes);
            if self.pending_through.as_deref() != Some(through)
                || index != self.next_fragment_index
                || !ReviewHandoff::valid_content_item(item)
                || self.next_fragment_index == crate::context::MAX_HANDOFF_FRAGMENTS
                || self.handoff_bytes
                    > crate::context::MAX_HANDOFF_LOGICAL_BYTES
                        + crate::context::MAX_HANDOFF_FRAGMENTS * 64
            {
                self.transaction_invalid = true;
            }
            self.next_fragment_index = self.next_fragment_index.saturating_add(1);
        } else if is_user_turn_boundary(item) {
            self.saw_user_boundary = true;
        }
    }

    pub(super) fn interrupt(&mut self) {
        self.pending_through = None;
        self.pending_items.clear();
        self.transaction_requires_commit = false;
        self.transaction_invalid = false;
        self.next_fragment_index = 0;
        self.handoff_bytes = 0;
        self.saw_handoff = false;
        self.saw_user_boundary = false;
    }
}

pub(super) fn filter_complete_review_handoff_history(
    items: impl IntoIterator<Item = ResponseItem>,
) -> Vec<ResponseItem> {
    let mut filter = ReviewHandoffHistoryFilter::new();
    items
        .into_iter()
        .flat_map(|item| filter.push(item))
        .collect()
}

fn consume_reports_through(pending: &mut VecDeque<PendingReviewReport>, item_id: &str) {
    if let Some(index) = pending.iter().position(|report| report.item_id == item_id) {
        pending.drain(..=index);
    }
}

pub(crate) fn rollback_surviving_items(rollout_items: &[RolloutItem]) -> Vec<&RolloutItem> {
    let mut surviving: Vec<&RolloutItem> = Vec::new();
    let mut user_positions = Vec::new();
    let mut review_boundaries = std::collections::HashSet::new();
    let mut explicit_review_boundary_active = false;
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
                if is_legacy_review_user_message(response_item) && explicit_review_boundary_active {
                    false
                } else {
                    !ReviewHandoff::is_consumption_marker(response_item)
                        && !ReviewHandoff::is_pending_report_marker(response_item)
                        && !ReviewHandoff::is_content_item(response_item)
                        && is_user_turn_boundary(response_item)
                }
            }
            RolloutItem::InterAgentCommunication(_) => true,
            RolloutItem::InterAgentCommunicationMetadata { .. } => false,
            RolloutItem::SessionMeta(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_) => false,
            RolloutItem::EventMsg(event) => {
                let review_boundary = match event {
                    EventMsg::EnteredReviewMode(entered) => {
                        entered.item_id.as_deref().or(entered.turn_id.as_deref())
                    }
                    EventMsg::ItemCompleted(completed) => match &completed.item {
                        TurnItem::EnteredReviewMode(entered) => Some(entered.id.as_str()),
                        _ => None,
                    },
                    _ => None,
                };
                let is_new_boundary =
                    review_boundary.is_some_and(|id| review_boundaries.insert(id.to_string()));
                if is_new_boundary {
                    explicit_review_boundary_active = true;
                }
                is_new_boundary
            }
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
        if matches!(
            item,
            RolloutItem::EventMsg(
                EventMsg::ExitedReviewMode(_)
                    | EventMsg::ItemCompleted(codex_protocol::protocol::ItemCompletedEvent {
                        item: TurnItem::ExitedReviewMode(_),
                        ..
                    })
            )
        ) {
            explicit_review_boundary_active = false;
        }
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

pub(crate) fn is_legacy_review_user_message(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message { id: Some(id), .. } if id == LEGACY_REVIEW_USER_MESSAGE_ID
    )
}

#[cfg(test)]
#[path = "review_handoff_tests.rs"]
mod tests;
