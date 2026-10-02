//! Ask-disposition UI and source-scoped transfer events.

use super::*;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;

impl ChatWidget {
    pub(super) fn open_handoff_decision_prompt(
        &mut self,
        pending: PendingHandoffPlan,
        trigger: HandoffTrigger,
    ) {
        let Some(source_thread_id) = self.thread_id else {
            return;
        };
        let Some(generation) = self.handoff_state.active_generation else {
            return;
        };
        let proceed_plan = pending.plan().to_string();
        let defer_plan = proceed_plan.clone();
        self.record_handoff_disposition(trigger, HandoffTelemetryDisposition::Ask);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Handoff plan ready".to_string()),
            subtitle: Some("Choose how to continue with the validated plan.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Clear and proceed".to_string(),
                    description: Some("Start a fresh thread and execute the plan now.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::StartHandoffTransfer {
                            source_thread_id,
                            generation,
                            plan: proceed_plan.clone(),
                            disposition: HandoffDisposition::Proceed,
                            trigger,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Clear and defer".to_string(),
                    description: Some("Start fresh and wait for your next prompt.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::StartHandoffTransfer {
                            source_thread_id,
                            generation,
                            plan: defer_plan.clone(),
                            disposition: HandoffDisposition::Defer,
                            trigger,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Stay in Handoff mode".to_string(),
                    description: Some("Keep refining the handoff in this thread.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::StayInHandoff {
                            source_thread_id,
                            generation,
                            trigger,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            on_cancel: Some(Box::new(move |tx| {
                tx.send(AppEvent::StayInHandoff {
                    source_thread_id,
                    generation,
                    trigger,
                });
            })),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(super) fn emit_handoff_transfer(
        &self,
        pending: PendingHandoffPlan,
        disposition: HandoffDisposition,
        trigger: HandoffTrigger,
    ) {
        let Some(source_thread_id) = self.thread_id else {
            return;
        };
        let Some(generation) = self.handoff_state.active_generation else {
            return;
        };
        self.app_event_tx.send(AppEvent::StartHandoffTransfer {
            source_thread_id,
            generation,
            plan: pending.into_plan(),
            disposition,
            trigger,
        });
    }
}
