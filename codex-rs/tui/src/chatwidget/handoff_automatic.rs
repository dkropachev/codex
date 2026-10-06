//! Opt-in automatic handoff built on the existing manual transfer path.

use super::*;

impl ChatWidget {
    pub(in crate::chatwidget) fn observe_automatic_handoff_usage(
        &mut self,
        info: &TokenUsageInfo,
        update: context_pressure::UsageUpdate<'_>,
    ) {
        let Some(threshold) = self
            .config
            .tui_auto_handoff_threshold_percent
            .map(i64::from)
        else {
            return;
        };
        let Some(used) = info.adjusted_active_context_percent() else {
            self.handoff_state.automatic_latched = false;
            return;
        };
        if used < threshold {
            if self.handoff_state.automatic_compaction_observed
                && self.handoff_state.active.as_ref().is_some_and(|active| {
                    active.trigger == HandoffTrigger::Automatic
                        && active.phase != HandoffPhase::Planning
                })
                && matches!(update, context_pressure::UsageUpdate::LiveServerTurn(_))
            {
                self.cancel_automatic_handoff();
                return;
            }
            if matches!(update, context_pressure::UsageUpdate::LiveServerTurn(_))
                && self.handoff_state.active.is_none()
            {
                self.handoff_state.automatic_latched = false;
                self.handoff_state.automatic_cancelled_until_rearm = false;
            }
            return;
        }
        if matches!(update, context_pressure::UsageUpdate::LiveServerTurn(_))
            && self.turn_lifecycle.agent_turn_running
            && self.active_mode_kind() == ModeKind::Default
            && self.handoff_state.active.is_none()
            && !self.handoff_state.automatic_cancelled_until_rearm
        {
            self.handoff_state.automatic_latched = true;
        }
    }

    pub(crate) fn request_automatic_handoff_check(&self) {
        if self.handoff_state.automatic_latched
            && let Some(thread_id) = self.thread_id
        {
            self.app_event_tx
                .send(AppEvent::AutomaticHandoffCandidate { thread_id });
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
        if self.active_mode_kind() == ModeKind::Default
            && self.handoff_state.active.is_none()
            && !self.handoff_state.automatic_cancelled_until_rearm
            && self
                .token_info
                .as_ref()
                .and_then(TokenUsageInfo::adjusted_active_context_percent)
                .is_some_and(|used| used >= threshold)
        {
            self.handoff_state.automatic_latched = true;
        }
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
            && !self.handoff_state.automatic_cancelled_until_rearm
            && self.handoff_state.active.is_none()
            && self.handoff_state.pending.is_none()
            && self.is_session_configured()
            && self.collaboration_modes_enabled()
            && self.active_mode_kind() == ModeKind::Default
            && self.handoff_local_blocker().is_none()
            && self
                .token_info
                .as_ref()
                .and_then(TokenUsageInfo::adjusted_active_context_percent)
                .is_some_and(|used| used >= threshold)
    }

    pub(crate) fn start_automatic_handoff(&mut self) {
        if !self.automatic_handoff_is_locally_eligible() {
            return;
        }
        self.handoff_state.automatic_latched = false;
        self.handoff_state.automatic_compaction_observed = false;
        self.handoff_state.next_generation = self.handoff_state.next_generation.wrapping_add(1);
        self.handoff_state.active = Some(ManualHandoff {
            trigger: HandoffTrigger::Automatic,
            phase: HandoffPhase::WrappingUp,
            disposition: HandoffDisposition::Proceed,
            generation: self.handoff_state.next_generation,
            owned_turn_id: None,
            owned_submission_items: None,
            owned_turn_completed: false,
            ready_plan_turn_id: None,
            ready_plan_text: None,
        });
        let (submitted, _) = self.submit_user_message_with_history_and_shell_escape_policy(
            UserMessage::from(crate::handoff::AUTOMATIC_WRAP_UP_PROMPT),
            UserMessageHistoryRecord::UserMessageText,
            ShellEscapePolicy::Disallow,
            UserMessageSource::Prompt,
        );
        if !submitted {
            self.cancel_automatic_handoff();
        }
    }

    pub(super) fn advance_automatic_wrap_up_after_successful_turn(&mut self) -> bool {
        let Some(active) = self.handoff_state.active.as_ref() else {
            return false;
        };
        if active.trigger != HandoffTrigger::Automatic {
            return false;
        }
        if active.phase != HandoffPhase::WrappingUp {
            return active.phase == HandoffPhase::AwaitingPlanning;
        }
        if !active.owned_turn_completed {
            return true;
        }
        let generation = active.generation;
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.clear_turn();
            active.phase = HandoffPhase::AwaitingPlanning;
        }
        if let Some(source_thread_id) = self.thread_id {
            self.app_event_tx
                .send(AppEvent::AdvanceAutomaticHandoffPlanning {
                    source_thread_id,
                    generation,
                });
        }
        true
    }

    pub(crate) fn continue_automatic_handoff_planning(&mut self, generation: u64) {
        if !self.handoff_state.active.as_ref().is_some_and(|active| {
            active.trigger == HandoffTrigger::Automatic
                && active.phase == HandoffPhase::AwaitingPlanning
                && active.generation == generation
        }) {
            return;
        }
        if self.active_mode_kind() != ModeKind::Default || self.handoff_local_blocker().is_some() {
            self.cancel_automatic_handoff();
            return;
        }
        let Some(mask) = crate::handoff::handoff_mask(self.model_catalog.as_ref()) else {
            self.cancel_automatic_handoff();
            return;
        };
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.phase = HandoffPhase::Planning;
        }
        self.set_collaboration_mask_from_user_action(mask);
        let (submitted, _) = self.submit_user_message_with_history_and_shell_escape_policy(
            UserMessage::from(crate::handoff::AUTOMATIC_PLANNING_PROMPT),
            UserMessageHistoryRecord::UserMessageText,
            ShellEscapePolicy::Disallow,
            UserMessageSource::Prompt,
        );
        if !submitted {
            self.cancel_automatic_handoff();
        }
    }

    pub(crate) fn cancel_automatic_handoff_at_app_gate(&mut self, generation: u64) {
        if self.handoff_state.active.as_ref().is_some_and(|active| {
            active.trigger == HandoffTrigger::Automatic && active.generation == generation
        }) {
            self.cancel_automatic_handoff();
        }
    }

    pub(super) fn cancel_automatic_handoff(&mut self) {
        self.handoff_state.active = None;
        self.handoff_state.automatic_latched = false;
        self.handoff_state.automatic_cancelled_until_rearm = true;
        self.handoff_state.automatic_compaction_observed = false;
        if crate::handoff::is_handoff_mask(self.active_collaboration_mask.as_ref())
            && let Some(mask) = collaboration_modes::default_mode_mask(self.model_catalog.as_ref())
        {
            self.set_collaboration_mask(mask);
        }
    }

    pub(in crate::chatwidget) fn note_automatic_handoff_compaction(
        &mut self,
        observation: context_pressure::CompactionObservation,
    ) {
        if observation == context_pressure::CompactionObservation::Live
            && self.handoff_state.active.as_ref().is_some_and(|active| {
                active.trigger == HandoffTrigger::Automatic
                    && active.phase != HandoffPhase::Planning
            })
        {
            self.handoff_state.automatic_compaction_observed = true;
        }
    }
}
