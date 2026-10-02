//! Failure and cancellation handling for handoff transitions.

use super::*;
use crate::handoff::HandoffPlanValidationError;

impl ChatWidget {
    pub(in crate::chatwidget) fn fail_handoff_mode_update(&mut self) {
        let Some(active) = self.handoff_state.active.take() else {
            return;
        };
        self.handoff_state.clear_owned_turn();
        self.handoff_state.active_generation = None;
        self.handoff_state.mode_active = false;
        self.update_collaboration_mode_indicator();
        if active.trigger == HandoffTrigger::Automatic {
            self.handoff_state.automatic_latched = false;
            self.handoff_state.automatic_cancelled_until_rearm = true;
        }
        self.add_error_message(
            "Handoff mode could not be applied. The source thread is unchanged; retry /handoff when collaboration settings are available."
                .to_string(),
        );
        HandoffTelemetryEvent::Failure {
            trigger: active.trigger,
            reason: HandoffTelemetryReason::ModeUnavailable,
        }
        .record(&self.session_telemetry);
    }

    pub(super) fn finish_handoff_without_transfer(&mut self, trigger: HandoffTrigger) {
        if trigger == HandoffTrigger::Automatic {
            self.add_info_message(
                "Automatic handoff ended without a plan.".to_string(),
                Some("The source thread was not cleared. Stay in Handoff mode to answer clarification or review completion.".to_string()),
            );
        }
    }

    pub(super) fn fail_handoff_plan(
        &mut self,
        trigger: HandoffTrigger,
        error: HandoffPlanValidationError,
    ) {
        let reason = match error {
            HandoffPlanValidationError::Empty => HandoffTelemetryReason::EmptyPlan,
            HandoffPlanValidationError::TooLarge { .. } => HandoffTelemetryReason::OversizedPlan,
        };
        self.add_error_message(error.to_string());
        HandoffTelemetryEvent::Failure { trigger, reason }.record(&self.session_telemetry);
        if trigger == HandoffTrigger::Automatic {
            self.handoff_state.active = None;
            self.return_to_default_after_automatic_handoff();
        }
    }

    pub(super) fn cancel_active_handoff(&mut self, reason: HandoffTelemetryReason) {
        let Some(active) = self.handoff_state.active.take() else {
            return;
        };
        self.handoff_state.clear_owned_turn();
        HandoffTelemetryEvent::Cancellation {
            trigger: active.trigger,
            reason,
        }
        .record(&self.session_telemetry);
        if active.trigger == HandoffTrigger::Automatic {
            self.add_info_message(
                "Automatic handoff cancelled.".to_string(),
                Some(
                    "The source thread was not cleared; use /handoff to retry manually."
                        .to_string(),
                ),
            );
            self.return_to_default_after_automatic_handoff();
        } else {
            self.add_info_message(
                "Handoff planning stopped.".to_string(),
                Some("The source thread was not cleared; retry in Handoff mode or use /handoff after returning to Default mode.".to_string()),
            );
            self.handoff_state.active = Some(active);
        }
    }

    pub(super) fn fail_active_handoff(&mut self, reason: HandoffTelemetryReason) {
        let Some(active) = self.handoff_state.active.take() else {
            return;
        };
        self.handoff_state.clear_owned_turn();
        self.add_error_message(
            "Handoff could not continue. The source thread is unchanged; retry /handoff or transfer the plan manually."
                .to_string(),
        );
        HandoffTelemetryEvent::Failure {
            trigger: active.trigger,
            reason,
        }
        .record(&self.session_telemetry);
        if active.trigger == HandoffTrigger::Automatic {
            self.return_to_default_after_automatic_handoff();
        } else {
            self.handoff_state.active = Some(active);
        }
    }
}
