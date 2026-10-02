//! Active-context hinting and automatic-handoff eligibility.

use super::*;

const CONTEXT_HANDOFF_HINT_PERCENT: i64 = 70;

impl ChatWidget {
    pub(crate) fn passive_handoff_state(&self) -> crate::handoff::PassiveHandoffState {
        crate::handoff::PassiveHandoffState {
            context_hint_shown: self.handoff_state.context_hint_shown,
            automatic_cancelled_until_rearm: self.handoff_state.automatic_cancelled_until_rearm,
            next_generation: self.handoff_state.next_generation,
        }
    }

    pub(crate) fn restore_passive_handoff_state(
        &mut self,
        state: crate::handoff::PassiveHandoffState,
    ) {
        self.handoff_state.context_hint_shown = state.context_hint_shown;
        self.handoff_state.automatic_cancelled_until_rearm = state.automatic_cancelled_until_rearm;
        self.handoff_state.next_generation = self
            .handoff_state
            .next_generation
            .max(state.next_generation);
        self.handoff_state.automatic_latched = false;
    }

    pub(in crate::chatwidget) fn observe_handoff_context_usage(&mut self, from_replay: bool) {
        let Some(used_percent) = self.handoff_context_used_percent() else {
            return;
        };
        if used_percent >= CONTEXT_HANDOFF_HINT_PERCENT && !self.handoff_state.context_hint_shown {
            self.handoff_state.context_hint_shown = true;
            self.add_info_message(
                "Context is getting full.".to_string(),
                Some("Use /handoff to continue safely in a fresh session.".to_string()),
            );
        }
        if used_percent < CONTEXT_HANDOFF_HINT_PERCENT {
            self.handoff_state.context_hint_shown = false;
        }
        if from_replay {
            return;
        }
        if used_percent < CONTEXT_HANDOFF_HINT_PERCENT {
            self.handoff_state.automatic_cancelled_until_rearm = false;
        }

        let Some(threshold) = self
            .config
            .tui_auto_handoff_threshold_percent
            .map(i64::from)
        else {
            return;
        };
        if used_percent < threshold {
            if self.handoff_state.active.is_none() {
                self.handoff_state.automatic_latched = false;
            }
            return;
        }
        if self.turn_lifecycle.agent_turn_running
            && self.active_mode_kind() == ModeKind::Default
            && !self.handoff_state.mode_active
            && !self.config.ephemeral
            && self.handoff_state.active.is_none()
            && !self.handoff_state.automatic_cancelled_until_rearm
        {
            self.handoff_state.automatic_latched = true;
        }
    }

    pub(in crate::chatwidget) fn note_context_compacted_for_handoff(&mut self, from_replay: bool) {
        if !from_replay && self.handoff_state.active.is_none() {
            match (
                self.config
                    .tui_auto_handoff_threshold_percent
                    .map(i64::from),
                self.handoff_context_used_percent(),
            ) {
                (Some(threshold), Some(used)) if used < threshold => {
                    self.handoff_state.automatic_latched = false;
                    self.handoff_state.automatic_cancelled_until_rearm = false;
                }
                (Some(_), Some(_)) => {}
                _ => {
                    self.handoff_state.automatic_latched = false;
                    self.handoff_state.automatic_cancelled_until_rearm = true;
                }
            }
        }
    }

    pub(in crate::chatwidget) fn qualify_automatic_handoff_after_live_completion(&mut self) {
        let Some(threshold) = self
            .config
            .tui_auto_handoff_threshold_percent
            .map(i64::from)
        else {
            return;
        };
        if self
            .handoff_context_used_percent()
            .is_some_and(|used| used >= threshold)
            && self.active_mode_kind() == ModeKind::Default
            && !self.handoff_state.mode_active
            && !self.config.ephemeral
            && self.handoff_state.active.is_none()
            && !self.handoff_state.automatic_cancelled_until_rearm
        {
            self.handoff_state.automatic_latched = true;
        }
    }

    fn handoff_context_used_percent(&self) -> Option<i64> {
        let info = self.token_info.as_ref()?;
        let context_window = info.model_context_window?;
        Some(
            100 - info
                .last_token_usage
                .percent_of_context_window_remaining(context_window)
                .clamp(0, 100),
        )
    }

    pub(crate) fn automatic_handoff_is_locally_eligible(&self) -> bool {
        let Some(threshold) = self
            .config
            .tui_auto_handoff_threshold_percent
            .map(i64::from)
        else {
            return false;
        };
        self.handoff_state.automatic_latched
            && self.handoff_state.active.is_none()
            && self.handoff_state.pending.is_none()
            && !self.handoff_state.mode_active
            && !self.config.ephemeral
            && self.active_mode_kind() == ModeKind::Default
            && !self.active_side_conversation
            && !self.blocks_direct_input
            && !self.is_user_turn_pending_or_running()
            && !self.bottom_pane.is_task_running()
            && self.composer_is_empty()
            && !self.input_queue.has_queued_follow_up_messages()
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
            && self
                .handoff_context_used_percent()
                .is_some_and(|used| used >= threshold)
    }

    pub(crate) fn request_automatic_handoff_check(&self) {
        if self.handoff_state.automatic_latched
            && let Some(thread_id) = self.thread_id
        {
            self.app_event_tx
                .send(AppEvent::AutomaticHandoffCandidate { thread_id });
        }
    }

    pub(crate) fn start_automatic_handoff(&mut self) -> bool {
        if !self.automatic_handoff_is_locally_eligible() {
            return false;
        }
        self.handoff_state.automatic_latched = false;
        self.handoff_state.active = Some(ActiveHandoff::automatic());
        self.handoff_state.begin_generation();
        self.record_handoff_trigger(HandoffTrigger::Automatic);
        let submitted = self
            .submit_user_message_with_shell_escape_policy(
                UserMessage::from(crate::handoff::AUTOMATIC_WRAP_UP_PROMPT),
                ShellEscapePolicy::Disallow,
            )
            .is_some();
        if !submitted {
            self.fail_active_handoff(HandoffTelemetryReason::WrapUpSubmission);
        }
        submitted
    }
}
