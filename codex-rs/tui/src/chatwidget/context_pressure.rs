//! One passive context-pressure hint per thread and compaction cycle.

use super::*;
use std::cmp::Ordering;
use std::sync::Mutex;

const HINT_THRESHOLD_PERCENT: i64 = 70;

#[derive(Default)]
pub(super) struct ContextPressureState {
    by_thread: HashMap<ThreadId, ContextPressureCycle>,
}

#[derive(Default)]
struct ContextPressureCycle {
    last_compaction: Option<CompactionItemKey>,
    latest_seen_item: Option<CompactionItemKey>,
    latest_replayed_turn_id: Option<String>,
    last_usage_turn_id: Option<String>,
    hint_turn_id: Option<String>,
    hint_item: Option<CompactionItemKey>,
    hint_turn_running: bool,
    hint_shown: bool,
    waiting_for_lower_usage: bool,
}

impl ContextPressureCycle {
    fn item_after_hint(
        &self,
        item: &CompactionItemKey,
        observation: CompactionObservation,
        had_previous: bool,
    ) -> bool {
        if !self.hint_shown {
            return true;
        }
        if let Some(hint_item) = self.hint_item.as_ref() {
            match item.order_after(hint_item) {
                Some(Ordering::Less | Ordering::Equal) => return false,
                Some(Ordering::Greater)
                    if Some(hint_item.turn_id.as_str()) == self.hint_turn_id.as_deref()
                        || (Some(item.turn_id.as_str()) == self.hint_turn_id.as_deref()
                            && self.hint_turn_running)
                        || observation != CompactionObservation::OrderedReplay =>
                {
                    return true;
                }
                Some(Ordering::Greater) | None => {}
            }
        }
        let order = self
            .hint_turn_id
            .as_deref()
            .and_then(|hint_turn_id| v7_turn_order(&item.turn_id, hint_turn_id));
        match order {
            Some(Ordering::Greater) => true,
            Some(Ordering::Less) => false,
            Some(Ordering::Equal) => {
                self.hint_turn_running || observation == CompactionObservation::Live
            }
            None => {
                observation == CompactionObservation::Live
                    || (observation == CompactionObservation::BufferedReplay && had_previous)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CompactionObservation {
    Live,
    BufferedReplay,
    OrderedReplay,
}

#[derive(Clone, Copy)]
pub(super) enum UsageUpdate<'a> {
    LiveServerTurn(&'a str),
    BufferedServerTurn(&'a str),
    AttachmentReplay(&'a str),
    Uncorrelated,
}

#[derive(Clone)]
pub(super) struct CompactionItemKey {
    pub(super) id: String,
    pub(super) turn_id: String,
}

impl CompactionItemKey {
    fn order_after(&self, previous: &Self) -> Option<Ordering> {
        // Legacy history can regenerate its implicit turn ID on reconstruction. Item IDs
        // retain their sequence, so compare them before using turn IDs as a fallback.
        if let (Some(current), Some(previous_id)) = (item_v7_id(&self.id), item_v7_id(&previous.id))
        {
            return Some(current.as_bytes().cmp(previous_id.as_bytes()));
        }
        if let (Some((current_prefix, current_number)), Some((previous_prefix, previous_number))) =
            (self.id.rsplit_once('-'), previous.id.rsplit_once('-'))
            && current_prefix == previous_prefix
            && let (Ok(current), Ok(previous)) = (
                current_number.parse::<u64>(),
                previous_number.parse::<u64>(),
            )
        {
            return Some(current.cmp(&previous));
        }
        if self.id.starts_with("item-") || previous.id.starts_with("item-") {
            return None;
        }
        v7_turn_order(&self.turn_id, &previous.turn_id).filter(|order| *order != Ordering::Equal)
    }
}

fn item_v7_id(id: &str) -> Option<uuid::Uuid> {
    let suffix = id.rsplit_once('_').map_or(id, |(_, suffix)| suffix);
    uuid::Uuid::parse_str(suffix)
        .ok()
        .filter(|id| id.get_version_num() == 7)
}

fn v7_turn_order(current: &str, previous: &str) -> Option<Ordering> {
    let (Ok(current), Ok(previous)) = (
        uuid::Uuid::parse_str(current),
        uuid::Uuid::parse_str(previous),
    ) else {
        return None;
    };
    (current.get_version_num() == 7 && previous.get_version_num() == 7)
        .then(|| current.as_bytes().cmp(previous.as_bytes()))
}

impl ChatWidget {
    pub(crate) fn inherit_context_pressure_state(&mut self, previous: &ChatWidget) {
        self.context_pressure_state = Arc::clone(&previous.context_pressure_state);
    }

    pub(super) fn record_context_pressure_item(&mut self, id: &str, turn_id: &str) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let mut state = self
            .context_pressure_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cycle = state.by_thread.entry(thread_id).or_default();
        let item = CompactionItemKey {
            id: id.to_string(),
            turn_id: turn_id.to_string(),
        };
        if cycle
            .latest_seen_item
            .as_ref()
            .is_none_or(|previous| item.order_after(previous) == Some(Ordering::Greater))
        {
            cycle.latest_seen_item = Some(item);
        }
    }

    pub(super) fn update_context_pressure_hint(
        &mut self,
        info: &TokenUsageInfo,
        update: UsageUpdate<'_>,
    ) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let Some(percent) = info.adjusted_active_context_percent() else {
            return;
        };
        let live_turn_id = match update {
            UsageUpdate::LiveServerTurn(turn_id)
            | UsageUpdate::BufferedServerTurn(turn_id)
            | UsageUpdate::AttachmentReplay(turn_id) => Some(turn_id.to_string()),
            UsageUpdate::Uncorrelated => self.turn_lifecycle.last_turn_id.clone(),
        };
        let hint_turn_running = self.turn_lifecycle.agent_turn_running;
        let show_hint = {
            let mut state = self
                .context_pressure_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cycle = state.by_thread.entry(thread_id).or_default();
            if let UsageUpdate::LiveServerTurn(turn_id)
            | UsageUpdate::BufferedServerTurn(turn_id)
            | UsageUpdate::AttachmentReplay(turn_id) = update
            {
                if cycle.last_usage_turn_id.as_deref().is_some_and(|previous| {
                    v7_turn_order(turn_id, previous) == Some(Ordering::Less)
                }) {
                    return;
                }
                cycle.last_usage_turn_id = Some(turn_id.to_string());
            }
            let is_post_compaction_usage = match update {
                UsageUpdate::LiveServerTurn(turn_id)
                | UsageUpdate::BufferedServerTurn(turn_id)
                | UsageUpdate::AttachmentReplay(turn_id) => cycle
                    .last_compaction
                    .as_ref()
                    .is_none_or(|last| match v7_turn_order(turn_id, &last.turn_id) {
                        Some(Ordering::Greater) => true,
                        Some(Ordering::Less) => false,
                        Some(Ordering::Equal) => {
                            !matches!(update, UsageUpdate::AttachmentReplay(_))
                        }
                        None => false,
                    }),
                UsageUpdate::Uncorrelated => true,
            };
            if cycle.waiting_for_lower_usage
                && is_post_compaction_usage
                && percent < HINT_THRESHOLD_PERCENT
            {
                cycle.waiting_for_lower_usage = false;
                cycle.hint_shown = false;
            }
            if percent >= HINT_THRESHOLD_PERCENT
                && !cycle.hint_shown
                && !cycle.waiting_for_lower_usage
            {
                cycle.hint_shown = true;
                cycle.hint_turn_id = live_turn_id.or_else(|| cycle.latest_replayed_turn_id.clone());
                cycle.hint_item = cycle.latest_seen_item.clone();
                cycle.hint_turn_running = hint_turn_running;
                true
            } else {
                false
            }
        };
        if show_hint {
            self.add_info_message(
                "Context use has reached 70%.".to_string(),
                Some(
                    "Use /compact to make room here, or /handoff to continue in a fresh thread."
                        .to_string(),
                ),
            );
        }
    }

    pub(super) fn observe_context_compaction(
        &mut self,
        id: &str,
        turn_id: &str,
        observation: CompactionObservation,
    ) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let mut state = self
            .context_pressure_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cycle = state.by_thread.entry(thread_id).or_default();
        let item = CompactionItemKey {
            id: id.to_string(),
            turn_id: turn_id.to_string(),
        };
        let had_previous = cycle.last_compaction.is_some();
        if let Some(last) = cycle.last_compaction.as_ref() {
            if last.id == id
                || (observation != CompactionObservation::OrderedReplay
                    && item.order_after(last) != Some(Ordering::Greater))
            {
                return;
            }
        } else if !cycle.hint_shown && observation == CompactionObservation::BufferedReplay {
            cycle.last_compaction = Some(item);
            return;
        }
        if !cycle.item_after_hint(&item, observation, had_previous) {
            cycle.last_compaction = Some(item);
            return;
        }
        cycle.last_compaction = Some(item);
        cycle.waiting_for_lower_usage = cycle.hint_shown;
    }

    pub(super) fn reconcile_replayed_compactions(
        &mut self,
        items: &[CompactionItemKey],
        latest_turn_id: Option<&str>,
    ) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let next_id = {
            let mut state = self
                .context_pressure_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let cycle = state.by_thread.entry(thread_id).or_default();
            if let Some(latest_turn_id) = latest_turn_id
                && cycle
                    .latest_replayed_turn_id
                    .as_deref()
                    .is_none_or(|previous| {
                        v7_turn_order(latest_turn_id, previous) == Some(Ordering::Greater)
                    })
            {
                cycle.latest_replayed_turn_id = Some(latest_turn_id.to_string());
            }
            match cycle.last_compaction.as_ref() {
                None if cycle.hint_shown => {
                    let candidate = items.last().cloned();
                    if candidate.as_ref().is_some_and(|last| {
                        cycle.item_after_hint(
                            last,
                            CompactionObservation::OrderedReplay,
                            /*had_previous*/ false,
                        )
                    }) {
                        candidate
                    } else {
                        cycle.last_compaction = candidate;
                        None
                    }
                }
                None => {
                    cycle.last_compaction = items.last().cloned();
                    None
                }
                Some(previous)
                    if items
                        .iter()
                        .rposition(|item| item.id == previous.id)
                        .is_some_and(|index| index + 1 < items.len()) =>
                {
                    items.last().cloned()
                }
                Some(previous)
                    if items.last().is_some_and(|last| {
                        last.order_after(previous) == Some(Ordering::Greater)
                    }) =>
                {
                    items.last().cloned()
                }
                Some(_) => None,
            }
        };
        if let Some(item) = next_id {
            self.observe_context_compaction(
                &item.id,
                &item.turn_id,
                CompactionObservation::OrderedReplay,
            );
        }
    }
}

pub(super) type SharedContextPressureState = Arc<Mutex<ContextPressureState>>;
