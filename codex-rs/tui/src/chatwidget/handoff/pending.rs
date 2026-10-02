//! Runtime-only pending handoff display, consumption, and discard confirmation.

use super::*;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;

impl ChatWidget {
    pub(crate) fn pending_handoff_plan(&self) -> Option<&PendingHandoffPlan> {
        self.handoff_state
            .pending
            .as_ref()
            .map(PendingHandoffState::plan)
    }

    pub(crate) fn pending_handoff_state(&self) -> Option<&PendingHandoffState> {
        self.handoff_state.pending.as_ref()
    }

    pub(crate) fn install_pending_handoff(&mut self, pending: PendingHandoffPlan) {
        self.handoff_state.pending = Some(PendingHandoffState::new(pending.clone()));
        self.display_pending_handoff(&pending, /*awaiting_commit*/ false);
    }

    pub(crate) fn restore_pending_handoff_for_replay(&mut self, pending: PendingHandoffState) {
        self.handoff_state.pending = Some(pending);
    }

    pub(crate) fn redisplay_pending_handoff(&mut self) {
        if let Some(pending) = self.handoff_state.pending.clone() {
            self.display_pending_handoff(
                pending.plan(),
                /*awaiting_commit*/ pending.submitted_text().is_some(),
            );
        }
    }

    fn display_pending_handoff(&mut self, pending: &PendingHandoffPlan, awaiting_commit: bool) {
        let (title, hint) = if awaiting_commit {
            (
                "Handoff execution awaiting confirmation",
                "The plan was submitted and will remain recoverable until the app server commits the exact prompt.",
            )
        } else {
            (
                "Pending handoff plan",
                "It will be sent with your next model-bound prompt; local and shell commands leave it pending.",
            )
        };
        self.add_info_message(title.to_string(), Some(hint.to_string()));
        let cwd = self.config.cwd.to_path_buf();
        self.add_to_history(history_cell::new_proposed_plan(
            pending.plan().to_string(),
            &cwd,
        ));
    }

    pub(crate) fn submit_handoff_execution(
        &mut self,
        pending: PendingHandoffPlan,
        trigger: HandoffTrigger,
    ) -> bool {
        let prompt = pending.execution_prompt();
        let submitted = self.submit_handoff_user_message(UserMessage::from(prompt.clone()));
        let mut pending = PendingHandoffState::proceeding(pending, trigger);
        if submitted {
            pending.mark_submitted(prompt);
        }
        self.handoff_state.pending = Some(pending);
        submitted
    }

    pub(in crate::chatwidget) fn pending_handoff_execution(
        &self,
        instruction: &str,
    ) -> Option<(String, usize)> {
        let pending = self.handoff_state.pending.as_ref()?.plan();
        if instruction.is_empty() {
            return Some((pending.execution_prompt(), 0));
        }
        let prompt = pending.execution_prompt_with_instruction(instruction);
        let instruction_offset = prompt.len().saturating_sub(instruction.len());
        Some((prompt, instruction_offset))
    }

    pub(in crate::chatwidget) fn mark_pending_handoff_submission_awaiting_commit(
        &mut self,
        submitted_text: String,
    ) {
        if let Some(pending) = self.handoff_state.pending.as_mut() {
            pending.mark_submitted(submitted_text);
        }
    }

    pub(in crate::chatwidget) fn pending_handoff_submission_failed(&mut self) {
        if let Some(pending) = self.handoff_state.pending.as_mut() {
            pending.mark_submission_failed();
        }
    }

    pub(in crate::chatwidget) fn accept_pending_handoff_submission(&mut self) {
        if self
            .handoff_state
            .pending
            .as_ref()
            .is_none_or(|pending| pending.submitted_text().is_none())
        {
            return;
        }
        self.consume_pending_handoff_submission();
    }

    fn consume_pending_handoff_submission(&mut self) {
        let completion = self
            .handoff_state
            .pending
            .as_ref()
            .and_then(PendingHandoffState::completion);
        if self.handoff_state.pending.take().is_some()
            && let Some(thread_id) = self.thread_id
        {
            self.app_event_tx.send(AppEvent::PendingHandoffConsumed {
                thread_id,
                completion,
            });
        }
    }

    pub(in crate::chatwidget) fn reconcile_committed_pending_handoff_submission(
        &mut self,
        items: &[UserInput],
    ) {
        let expected = self
            .handoff_state
            .pending
            .as_ref()
            .and_then(PendingHandoffState::submitted_text);
        let matched = items.iter().any(
            |item| matches!(item, UserInput::Text { text, .. } if Some(text.as_str()) == expected),
        );
        if matched {
            self.accept_pending_handoff_submission();
        }
    }

    pub(crate) fn reconcile_replayed_pending_handoff_submission(&mut self, matched: bool) {
        if matched {
            self.consume_pending_handoff_submission();
        }
    }

    pub(in crate::chatwidget) fn confirm_clear_pending_handoff(
        &mut self,
        name: Option<String>,
    ) -> bool {
        self.confirm_pending_handoff_discard(
            "Starting another fresh chat removes this in-memory handoff plan.",
            "Discard and clear",
            "Start another fresh chat without the plan.",
            move |tx| tx.send(AppEvent::ClearUi { name: name.clone() }),
        )
    }

    pub(crate) fn confirm_new_pending_handoff(&mut self, name: Option<String>) -> bool {
        self.confirm_pending_handoff_discard(
            "Starting another chat removes this in-memory handoff plan.",
            "Discard and start new",
            "Start another chat without the plan.",
            move |tx| tx.send(AppEvent::NewSession { name: name.clone() }),
        )
    }

    pub(crate) fn confirm_misalignment_new_pending_handoff(
        &mut self,
        name: Option<String>,
    ) -> bool {
        if self.handoff_state.pending.is_none() {
            return false;
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Discard the pending handoff?".to_string()),
            subtitle: Some(
                "Starting another chat removes this in-memory handoff plan.".to_string(),
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Keep the pending handoff".to_string(),
                    description: Some("Return to the precaution choices.".to_string()),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::RestoreMisalignmentPrecaution);
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Discard and start new".to_string(),
                    description: Some("Start another chat without the plan.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::NewSession { name: name.clone() });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            on_cancel: Some(Box::new(|tx| {
                tx.send(AppEvent::RestoreMisalignmentPrecaution);
            })),
            ..Default::default()
        });
        self.request_redraw();
        true
    }

    fn confirm_pending_handoff_discard(
        &mut self,
        subtitle: &'static str,
        action_name: &'static str,
        action_description: &'static str,
        action: impl Fn(&AppEventSender) + Send + Sync + 'static,
    ) -> bool {
        if self.handoff_state.pending.is_none() {
            return false;
        }
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Discard the pending handoff?".to_string()),
            subtitle: Some(subtitle.to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Keep the pending handoff".to_string(),
                    description: Some("Return to this fresh session.".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: action_name.to_string(),
                    description: Some(action_description.to_string()),
                    actions: vec![Box::new(action)],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        self.request_redraw();
        true
    }

    pub(in crate::chatwidget) fn has_pending_handoff(&self) -> bool {
        self.handoff_state.pending.is_some()
    }

    pub(crate) fn discard_pending_handoff(&mut self) {
        self.handoff_state.pending = None;
    }
}

impl ThreadInputState {
    pub(crate) fn pending_handoff_submission_text(&self) -> Option<&str> {
        self.handoff_state
            .pending
            .as_ref()
            .and_then(PendingHandoffState::submitted_text)
    }

    pub(crate) fn replace_pending_handoff_state(&mut self, pending: Option<PendingHandoffState>) {
        self.handoff_state.pending = pending;
    }

    pub(crate) fn recover_pending_handoff_submission(&mut self, prompt: &UserMessage) -> bool {
        let Some(pending) = self.handoff_state.pending.as_mut() else {
            return false;
        };
        if pending.submitted_text() != Some(prompt.text.as_str()) {
            return false;
        }
        pending.mark_submission_failed();
        self.user_turn_pending_start = false;
        true
    }
}
