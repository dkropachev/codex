//! Handoff planning, deferred-plan, and context-threshold state for `ChatWidget`.

mod automatic;
mod context;
mod decision;
mod failure;
mod pending;
mod replay;

use super::*;
use crate::handoff::ActiveHandoff;
use crate::handoff::HandoffDisposition;
use crate::handoff::HandoffPhase;
use crate::handoff::HandoffTelemetryDisposition;
use crate::handoff::HandoffTelemetryEvent;
use crate::handoff::HandoffTelemetryReason;
use crate::handoff::HandoffTrigger;
use crate::handoff::PendingHandoffPlan;
use crate::handoff::PendingHandoffState;

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct HandoffState {
    active: Option<ActiveHandoff>,
    pending: Option<PendingHandoffState>,
    mode_active: bool,
    context_hint_shown: bool,
    automatic_latched: bool,
    automatic_cancelled_until_rearm: bool,
    next_generation: u64,
    active_generation: Option<u64>,
    owned_turn_id: Option<String>,
    owned_turn_completed: bool,
    owned_submission_items: Option<Vec<UserInput>>,
    owned_submission_after_turn_id: Option<String>,
}

impl HandoffState {
    fn begin_generation(&mut self) {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.active_generation = Some(self.next_generation);
        self.clear_owned_turn();
    }

    fn clear_owned_turn(&mut self) {
        self.owned_turn_id = None;
        self.owned_turn_completed = false;
        self.owned_submission_items = None;
        self.owned_submission_after_turn_id = None;
    }
}

impl ChatWidget {
    pub(super) fn begin_manual_handoff(
        &mut self,
        disposition: HandoffDisposition,
        user_message: UserMessage,
    ) -> bool {
        if !self.collaboration_modes_enabled() {
            self.add_info_message(
                "Collaboration modes are disabled.".to_string(),
                Some("Enable collaboration modes to use /handoff.".to_string()),
            );
            return false;
        }
        if self.active_mode_kind() != ModeKind::Default || self.handoff_state.mode_active {
            self.add_error_message(
                "/handoff is available only from an idle Default-mode session.".to_string(),
            );
            return false;
        }
        if self.active_side_conversation
            || self.app_overlay_active
            || self.is_user_turn_pending_or_running()
            || self.bottom_pane.is_task_running()
            || !self.composer_is_empty()
            || self.has_queued_follow_up_messages()
            || !self.input_queue.pending_steers.is_empty()
            || self.input_queue.suppress_queue_autosend
            || self.input_queue.rate_limit_recovery_pending
            || self.input_queue.recovered_queue
            || !self.bottom_pane.no_modal_or_popup_active()
            || self.has_pending_protected_request()
            || matches!(
                self.rate_limit_switch_prompt,
                RateLimitSwitchPromptState::Pending
            )
            || self
                .current_goal_status
                .as_ref()
                .is_some_and(GoalStatusState::is_active)
        {
            self.add_error_message(
                "/handoff is available only from an idle Default-mode session.".to_string(),
            );
            return false;
        }
        if self.handoff_state.pending.is_some() {
            self.add_error_message(
                "A handoff plan is already pending; submit an instruction or confirm discarding it before starting another handoff."
                    .to_string(),
            );
            return false;
        }
        if self.config.ephemeral {
            self.add_error_message(
                "/handoff requires a resumable source thread; this session is ephemeral."
                    .to_string(),
            );
            return false;
        }
        if self.blocks_direct_input {
            self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            return false;
        }
        let Some(mask) = crate::handoff::handoff_mask(self.model_catalog.as_ref()) else {
            self.add_error_message("Handoff mode is unavailable right now.".to_string());
            return false;
        };

        self.handoff_state.active = Some(ActiveHandoff::manual(disposition));
        self.handoff_state.begin_generation();
        self.handoff_state.mode_active = true;
        self.handoff_state.automatic_latched = false;
        self.record_handoff_trigger(HandoffTrigger::Manual);
        self.set_collaboration_mask_from_user_action(mask);
        let submitted = self
            .submit_user_message_with_shell_escape_policy(user_message, ShellEscapePolicy::Disallow)
            .is_some();
        if !submitted {
            self.fail_active_handoff(HandoffTelemetryReason::Submission);
        }
        submitted
    }

    pub(super) fn handoff_mode_active(&self) -> bool {
        self.handoff_state.mode_active
    }

    pub(crate) fn is_current_handoff_source(&self, source_thread_id: ThreadId) -> bool {
        self.thread_id == Some(source_thread_id)
            && self.handoff_state.mode_active
            && self.active_mode_kind() == ModeKind::Plan
    }

    pub(crate) fn is_current_handoff_transaction(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        self.thread_id == Some(source_thread_id)
            && self.handoff_state.active.is_some()
            && self.handoff_state.active_generation == Some(generation)
    }

    #[cfg(test)]
    pub(crate) fn active_handoff_generation(&self) -> Option<u64> {
        self.handoff_state.active_generation
    }

    pub(crate) fn advance_restored_manual_handoff_after_snapshot(&mut self) {
        let can_advance = self.handoff_state.active.is_some_and(|active| {
            active.trigger == HandoffTrigger::Manual && active.phase == HandoffPhase::Planning
        }) && self.handoff_state.owned_turn_completed
            && self.handoff_state.owned_turn_id
                == self.transcript.latest_authoritative_plan_turn_id;
        if can_advance {
            self.advance_handoff_after_successful_turn();
        }
    }

    pub(super) fn note_handoff_submission(&mut self, items: Vec<UserInput>) {
        if self.handoff_state.active.is_some_and(|active| {
            matches!(
                active.phase,
                HandoffPhase::WrappingUp | HandoffPhase::Planning
            )
        }) {
            let previous_turn_id = self.turn_lifecycle.last_turn_id.clone();
            let running_turn_id = self
                .turn_lifecycle
                .agent_turn_running
                .then(|| previous_turn_id.clone())
                .flatten();
            self.handoff_state.clear_owned_turn();
            self.handoff_state.owned_submission_items = Some(items);
            self.handoff_state.owned_turn_id = running_turn_id;
            self.handoff_state.owned_submission_after_turn_id = self
                .handoff_state
                .owned_turn_id
                .is_none()
                .then_some(previous_turn_id)
                .flatten();
        }
    }

    pub(in crate::chatwidget) fn handoff_tracks_submission(&self) -> bool {
        self.handoff_state.active.is_some_and(|active| {
            matches!(
                active.phase,
                HandoffPhase::WrappingUp | HandoffPhase::Planning
            )
        })
    }

    pub(crate) fn bind_handoff_turn_start(&mut self, turn_id: &str, items: &[UserInput]) {
        if self.handoff_state.owned_submission_items.as_deref() == Some(items) {
            self.handoff_state.owned_submission_items = None;
            self.handoff_state.owned_turn_id = Some(turn_id.to_string());
            self.handoff_state.owned_turn_completed = false;
            self.handoff_state.owned_submission_after_turn_id = None;
        }
    }

    pub(super) fn note_handoff_turn_completed(&mut self, turn_id: &str) {
        if self.handoff_state.owned_submission_items.is_none()
            && self.handoff_state.owned_turn_id.as_deref() == Some(turn_id)
        {
            self.handoff_state.owned_turn_completed = true;
        }
    }

    pub(super) fn leave_handoff_for_user_mode_change(&mut self, mask: &CollaborationModeMask) {
        let automatic_active = self
            .handoff_state
            .active
            .is_some_and(|active| active.trigger == HandoffTrigger::Automatic);
        if (self.handoff_state.mode_active || automatic_active)
            && !crate::handoff::is_handoff_mask(Some(mask))
        {
            if let Some(active) = self.handoff_state.active.take() {
                HandoffTelemetryEvent::Cancellation {
                    trigger: active.trigger,
                    reason: HandoffTelemetryReason::ModeChange,
                }
                .record(&self.session_telemetry);
                if active.trigger == HandoffTrigger::Automatic {
                    self.handoff_state.automatic_latched = false;
                    self.handoff_state.automatic_cancelled_until_rearm = true;
                    self.add_info_message(
                        "Automatic handoff cancelled by the mode change.".to_string(),
                        Some("The source thread will not be cleared automatically.".to_string()),
                    );
                }
            }
            self.handoff_state.clear_owned_turn();
            self.handoff_state.active_generation = None;
            self.handoff_state.mode_active = false;
        }
    }

    pub(crate) fn stay_in_handoff(&mut self, generation: u64, trigger: HandoffTrigger) {
        if self.handoff_state.active_generation != Some(generation) {
            return;
        }
        self.record_handoff_disposition(trigger, HandoffTelemetryDisposition::Stay);
        self.handoff_state.active = Some(ActiveHandoff {
            trigger,
            disposition: HandoffDisposition::Ask,
            phase: HandoffPhase::Planning,
        });
        self.handoff_state.begin_generation();
        self.handoff_state.mode_active = true;
    }

    pub(crate) fn resume_handoff_after_transfer_failure(
        &mut self,
        trigger: HandoffTrigger,
        disposition: HandoffDisposition,
    ) {
        if trigger == HandoffTrigger::Automatic {
            self.handoff_state.active = None;
            self.return_to_default_after_automatic_handoff();
            return;
        }
        self.handoff_state.begin_generation();
        self.handoff_state.active = Some(ActiveHandoff {
            trigger,
            disposition,
            phase: HandoffPhase::Planning,
        });
        self.handoff_state.mode_active = true;
    }

    /// Handles a successful live turn owned by the handoff state machine.
    ///
    /// Returns true when generic Plan-mode completion UI must be suppressed.
    pub(super) fn advance_handoff_after_successful_turn(&mut self) -> bool {
        let Some(active) = self.handoff_state.active else {
            return false;
        };
        if self.has_queued_follow_up_messages() || !self.input_queue.pending_steers.is_empty() {
            self.cancel_active_handoff(HandoffTelemetryReason::QueuedInput);
            return true;
        }
        if active.phase == HandoffPhase::AwaitingPlanning
            || !self.handoff_state.owned_turn_completed
        {
            return false;
        }
        if active.trigger == HandoffTrigger::Automatic
            && self.automatic_handoff_transition_is_blocked(active.phase)
        {
            self.cancel_active_handoff(HandoffTelemetryReason::TransitionPreempted);
            return true;
        }
        match active.phase {
            HandoffPhase::WrappingUp => {
                self.handoff_state.clear_owned_turn();
                if let Some(active) = self.handoff_state.active.as_mut() {
                    active.await_planning_gate();
                }
                if let (Some(source_thread_id), Some(generation)) =
                    (self.thread_id, self.handoff_state.active_generation)
                {
                    self.app_event_tx
                        .send(AppEvent::AdvanceAutomaticHandoffPlanning {
                            source_thread_id,
                            generation,
                        });
                }
                true
            }
            HandoffPhase::AwaitingPlanning => true,
            HandoffPhase::Planning => {
                let plan = self
                    .handoff_state
                    .owned_turn_id
                    .eq(&self.transcript.latest_authoritative_plan_turn_id)
                    .then(|| {
                        self.transcript
                            .latest_authoritative_plan_markdown
                            .clone()
                            .unwrap_or_default()
                    });
                self.handoff_state.clear_owned_turn();
                let pending = match plan {
                    Some(plan) => PendingHandoffPlan::new(plan),
                    None => {
                        self.finish_handoff_without_transfer(active.trigger);
                        return true;
                    }
                };
                let pending = match pending {
                    Ok(pending) => pending,
                    Err(error) => {
                        self.fail_handoff_plan(active.trigger, error);
                        return true;
                    }
                };
                match active.disposition {
                    HandoffDisposition::Proceed => {
                        self.emit_handoff_transfer(
                            pending,
                            HandoffDisposition::Proceed,
                            active.trigger,
                        );
                    }
                    HandoffDisposition::Defer => {
                        self.emit_handoff_transfer(
                            pending,
                            HandoffDisposition::Defer,
                            active.trigger,
                        );
                    }
                    HandoffDisposition::Ask => {
                        self.open_handoff_decision_prompt(pending, active.trigger)
                    }
                }
                true
            }
        }
    }

    pub(crate) fn handoff_transfer_is_locally_safe(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
        trigger: HandoffTrigger,
        disposition: HandoffDisposition,
    ) -> bool {
        self.thread_id == Some(source_thread_id)
            && self.handoff_state.active_generation == Some(generation)
            && self.handoff_state.active.is_some_and(|active| {
                active.trigger == trigger
                    && (active.disposition == disposition
                        || (active.disposition == HandoffDisposition::Ask
                            && disposition != HandoffDisposition::Ask))
                    && active.phase == HandoffPhase::Planning
            })
            && self.handoff_state.mode_active
            && self.active_mode_kind() == ModeKind::Plan
            && !self.active_side_conversation
            && !self.app_overlay_active
            && !self.blocks_direct_input
            && !self.is_user_turn_pending_or_running()
            && !self.bottom_pane.is_task_running()
            && self.composer_is_empty()
            && !self.has_queued_follow_up_messages()
            && self.input_queue.pending_steers.is_empty()
            && !self.input_queue.suppress_queue_autosend
            && !self.input_queue.rate_limit_recovery_pending
            && !self.input_queue.recovered_queue
            && self.bottom_pane.no_modal_or_popup_active()
            && !self.has_pending_protected_request()
            && !matches!(
                self.rate_limit_switch_prompt,
                RateLimitSwitchPromptState::Pending
            )
            && !self
                .current_goal_status
                .as_ref()
                .is_some_and(GoalStatusState::is_active)
    }

    fn record_handoff_trigger(&self, trigger: HandoffTrigger) {
        HandoffTelemetryEvent::Trigger(trigger).record(&self.session_telemetry);
    }

    pub(super) fn record_handoff_disposition(
        &self,
        trigger: HandoffTrigger,
        disposition: HandoffTelemetryDisposition,
    ) {
        HandoffTelemetryEvent::Disposition {
            trigger,
            disposition,
        }
        .record(&self.session_telemetry);
    }
}
