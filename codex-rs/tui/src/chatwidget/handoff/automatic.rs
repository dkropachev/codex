//! Automatic handoff transitions and cancellation policy.

use super::*;

impl ChatWidget {
    pub(super) fn automatic_handoff_transition_is_blocked(&self, phase: HandoffPhase) -> bool {
        let mode_changed = match phase {
            HandoffPhase::WrappingUp => {
                self.active_mode_kind() != ModeKind::Default || self.handoff_state.mode_active
            }
            HandoffPhase::Planning => {
                self.active_mode_kind() != ModeKind::Plan || !self.handoff_state.mode_active
            }
            HandoffPhase::AwaitingPlanning => self.active_mode_kind() != ModeKind::Default,
        };
        mode_changed
            || self.active_side_conversation
            || self.app_overlay_active
            || self.blocks_direct_input
            || self.bottom_pane.is_task_running()
            || !self.composer_is_empty()
            || self.has_queued_follow_up_messages()
            || !self.input_queue.pending_steers.is_empty()
            || self.input_queue.user_turn_pending_start
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
    }

    pub(crate) fn continue_automatic_handoff_planning(&mut self, generation: u64) -> bool {
        let valid = self.handoff_state.active_generation == Some(generation)
            && self.handoff_state.active.is_some_and(|active| {
                active.trigger == HandoffTrigger::Automatic
                    && active.phase == HandoffPhase::AwaitingPlanning
            });
        if !valid {
            return false;
        }
        if self.automatic_handoff_transition_is_blocked(HandoffPhase::AwaitingPlanning) {
            self.cancel_active_handoff(HandoffTelemetryReason::PlanningGate);
            return false;
        }
        let Some(mask) = crate::handoff::handoff_mask(self.model_catalog.as_ref()) else {
            self.fail_active_handoff(HandoffTelemetryReason::ModeUnavailable);
            return false;
        };
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.begin_planning();
        }
        self.handoff_state.mode_active = true;
        self.set_collaboration_mask_from_user_action(mask);
        let submitted = self
            .submit_user_message_with_shell_escape_policy(
                UserMessage::from(crate::handoff::AUTOMATIC_PLANNING_PROMPT),
                ShellEscapePolicy::Disallow,
            )
            .is_some();
        if !submitted {
            self.fail_active_handoff(HandoffTelemetryReason::PlanningSubmission);
        }
        submitted
    }

    pub(crate) fn cancel_automatic_handoff_at_app_gate(&mut self, generation: u64) {
        if self.handoff_state.active_generation == Some(generation) {
            self.cancel_active_handoff(HandoffTelemetryReason::PlanningGate);
        }
    }

    pub(in crate::chatwidget) fn cancel_handoff_for_unsuccessful_turn(
        &mut self,
        reason: HandoffTelemetryReason,
    ) {
        if self.thread_usage.replaying_turn_completion {
            return;
        }
        if self.handoff_state.active.is_some() {
            self.cancel_active_handoff(reason);
        } else if self.handoff_state.automatic_latched {
            self.handoff_state.automatic_latched = false;
            self.handoff_state.automatic_cancelled_until_rearm = true;
            HandoffTelemetryEvent::Cancellation {
                trigger: HandoffTrigger::Automatic,
                reason,
            }
            .record(&self.session_telemetry);
        }
    }

    pub(crate) fn cancel_automatic_handoff_for_navigation(&mut self) {
        if self
            .handoff_state
            .active
            .is_some_and(|active| active.trigger == HandoffTrigger::Automatic)
        {
            self.cancel_active_handoff(HandoffTelemetryReason::ThreadNavigation);
        } else if self.handoff_state.automatic_latched {
            self.handoff_state.automatic_latched = false;
            self.handoff_state.automatic_cancelled_until_rearm = true;
            HandoffTelemetryEvent::Cancellation {
                trigger: HandoffTrigger::Automatic,
                reason: HandoffTelemetryReason::ThreadNavigation,
            }
            .record(&self.session_telemetry);
        }
    }

    pub(in crate::chatwidget) fn cancel_replayed_handoff_turn_if_owned(
        &mut self,
        turn_id: &str,
        reason: HandoffTelemetryReason,
    ) {
        if self.handoff_state.owned_turn_id.as_deref() == Some(turn_id) {
            self.cancel_active_handoff(reason);
        }
    }

    pub(super) fn return_to_default_after_automatic_handoff(&mut self) {
        self.handoff_state.automatic_cancelled_until_rearm = true;
        self.handoff_state.clear_owned_turn();
        self.handoff_state.active_generation = None;
        self.handoff_state.mode_active = false;
        if let Some(mask) = collaboration_modes::default_mode_mask(self.model_catalog.as_ref()) {
            self.set_collaboration_mask_from_user_action(mask);
        }
    }
}
