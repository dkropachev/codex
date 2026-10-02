//! Reconciliation of handoff turn ownership across detached snapshot replay.

use super::*;

impl ThreadInputState {
    pub(crate) fn reconcile_handoff_turns(&mut self, turns: &[Turn]) {
        let Some(active) = self.handoff_state.active else {
            return;
        };
        if active.trigger != HandoffTrigger::Manual || active.phase != HandoffPhase::Planning {
            return;
        }

        let expected_items = self.handoff_state.owned_submission_items.as_deref();
        let owned_turn_id = self.handoff_state.owned_turn_id.as_deref();
        let matching_turn = if let Some(owned_turn_id) = owned_turn_id {
            turns.iter().find(|turn| {
                turn.id == owned_turn_id
                    && expected_items.is_none_or(|expected| {
                        turn.items.iter().any(|item| {
                            let ThreadItem::UserMessage { content, .. } = item else {
                                return false;
                            };
                            content == expected
                        })
                    })
            })
        } else {
            let candidates = match self.handoff_state.owned_submission_after_turn_id.as_deref() {
                Some(previous_turn_id) => turns
                    .iter()
                    .position(|turn| turn.id == previous_turn_id)
                    .map(|index| &turns[index + 1..])
                    .unwrap_or_default(),
                None => turns,
            };
            candidates.iter().rev().find(|turn| {
                expected_items.is_some_and(|expected| {
                    turn.items.iter().any(|item| {
                        let ThreadItem::UserMessage { content, .. } = item else {
                            return false;
                        };
                        content == expected
                    })
                })
            })
        };
        let Some(turn) = matching_turn else {
            return;
        };
        self.handoff_state.owned_submission_items = None;

        match turn.status {
            TurnStatus::Completed => {
                self.handoff_state.owned_turn_id = Some(turn.id.clone());
                self.handoff_state.owned_turn_completed = true;
            }
            TurnStatus::InProgress => {
                self.handoff_state.owned_turn_id = Some(turn.id.clone());
                self.handoff_state.owned_turn_completed = false;
            }
            TurnStatus::Failed | TurnStatus::Interrupted => {
                self.handoff_state.clear_owned_turn();
            }
        }
    }
}
