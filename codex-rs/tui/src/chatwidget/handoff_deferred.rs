//! Pending handoff state after a completed Plan and before fresh-thread execution.

use super::*;
use crate::app_event::DeferredDiscardAction;

impl ChatWidget {
    pub(crate) fn has_pending_deferred_handoff(&self) -> bool {
        self.handoff_state.pending.is_some()
    }

    pub(crate) fn mark_deferred_execution_prompt(&mut self, user_message: UserMessage) {
        self.handoff_state.recoverable_execution_prompt = Some(user_message);
    }

    pub(in crate::chatwidget) fn fail_deferred_mode_update(&mut self) {
        if let Some(pending) = self.handoff_state.pending.take() {
            if let Some(user_message) = pending.in_flight {
                self.restore_user_message_to_composer(user_message);
            }
            self.refresh_deferred_footer();
            self.add_error_message(
                "Deferred handoff could not return to Default mode. Select Default mode and retry /handoff --defer."
                    .to_string(),
            );
        }
    }

    pub(in crate::chatwidget) fn clear_deferred_execution_prompt_after_turn_start(&mut self) {
        self.handoff_state.recoverable_execution_prompt = None;
    }

    pub(in crate::chatwidget) fn restore_rejected_deferred_execution_prompt(&mut self) {
        if let Some(user_message) = self.handoff_state.recoverable_execution_prompt.take() {
            self.restore_user_message_to_composer(user_message);
            self.add_info_message(
                "The fresh handoff turn was rejected.".to_string(),
                Some("The combined plan and prompt are in the composer for review.".to_string()),
            );
        }
    }

    pub(super) fn defer_handoff(
        &mut self,
        generation: u64,
        plan_turn_id: String,
        plan_text: String,
    ) {
        let Some(source_thread_id) = self.thread_id else {
            return;
        };
        let Some(default_mask) =
            collaboration_modes::default_mode_mask(self.model_catalog.as_ref())
        else {
            self.add_error_message(
                "Handoff planning finished, but Default mode is unavailable. Continue in Handoff mode and retry."
                    .to_string(),
            );
            return;
        };
        self.handoff_state.pending = Some(DeferredHandoff {
            source_thread_id,
            plan_turn_id,
            plan_text,
            generation,
            in_flight: None,
            awaiting_default_mode_update: false,
        });
        self.handoff_state.active = None;
        self.set_collaboration_mask_from_user_action(default_mask);
        if let Some(pending) = self.handoff_state.pending.as_mut() {
            pending.awaiting_default_mode_update = true;
        }
        self.refresh_deferred_footer();
        self.add_info_message(
            "Handoff plan ready for the next prompt.".to_string(),
            Some(
                "Local commands stay in this thread. Your next model-bound prompt starts a fresh thread with the plan."
                    .to_string(),
            ),
        );
        self.request_redraw();
    }

    pub(in crate::chatwidget) fn route_deferred_handoff_prompt(
        &mut self,
        user_message: UserMessage,
        source: UserMessageSource,
    ) -> DeferredSubmission {
        if source != UserMessageSource::Prompt {
            return DeferredSubmission::NotApplicable;
        }
        let Some(pending) = self.handoff_state.pending.as_mut() else {
            return DeferredSubmission::NotApplicable;
        };
        if self.thread_id != Some(pending.source_thread_id) {
            self.handoff_state.pending = None;
            return DeferredSubmission::NotApplicable;
        }
        if pending.in_flight.is_some() || self.turn_lifecycle.agent_turn_running {
            self.restore_user_message_to_composer(user_message);
            self.add_info_message(
                "The deferred handoff is waiting for the source thread to become idle.".to_string(),
                Some("Your prompt remains in the composer.".to_string()),
            );
            return DeferredSubmission::Blocked;
        }
        pending.in_flight = Some(user_message.clone());
        self.app_event_tx
            .send(AppEvent::StartDeferredHandoffTransfer {
                source_thread_id: pending.source_thread_id,
                plan_turn_id: pending.plan_turn_id.clone(),
                generation: pending.generation,
                plan: pending.plan_text.clone(),
                user_message,
            });
        self.refresh_deferred_footer();
        self.add_info_message(
            "Starting the deferred handoff in a fresh thread…".to_string(),
            Some("The source thread remains resumable.".to_string()),
        );
        DeferredSubmission::Queued
    }

    pub(crate) fn is_current_deferred_transaction(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        self.thread_id == Some(source_thread_id)
            && self.handoff_state.pending.as_ref().is_some_and(|pending| {
                pending.source_thread_id == source_thread_id && pending.generation == generation
            })
    }

    pub(crate) fn deferred_transfer_is_locally_safe(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
        plan: &str,
        user_message: &UserMessage,
    ) -> bool {
        self.is_current_deferred_transaction(source_thread_id, generation)
            && self.handoff_state.pending.as_ref().is_some_and(|pending| {
                pending.plan_text == plan
                    && pending.in_flight.as_ref() == Some(user_message)
                    && self.transcript.latest_authoritative_plan_turn_id.as_deref()
                        == Some(pending.plan_turn_id.as_str())
                    && self
                        .transcript
                        .latest_authoritative_plan_markdown
                        .as_deref()
                        == Some(plan)
            })
            && self.handoff_local_blocker().is_none()
    }

    pub(crate) fn rollback_deferred_handoff(&mut self, generation: u64) {
        if let Some(pending) = self.handoff_state.pending.as_mut()
            && pending.generation == generation
            && let Some(user_message) = pending.in_flight.take()
        {
            self.restore_user_message_to_composer(user_message);
            self.refresh_deferred_footer();
            self.request_redraw();
        }
    }

    pub(in crate::chatwidget) fn invalidate_deferred_handoff_after_accepted_input(
        &mut self,
        from_replay: bool,
    ) {
        let Some(pending) = self.handoff_state.pending.as_ref() else {
            return;
        };
        if self.transcript.latest_authoritative_plan_turn_id.as_deref()
            != Some(pending.plan_turn_id.as_str())
        {
            return;
        }
        let in_flight = self
            .handoff_state
            .pending
            .take()
            .and_then(|pending| pending.in_flight);
        if let Some(user_message) = in_flight {
            self.restore_user_message_to_composer(user_message);
        }
        self.refresh_deferred_footer();
        if !from_replay {
            self.add_info_message(
                "The deferred handoff plan was superseded by new source input.".to_string(),
                Some("Run /handoff --defer again to prepare a current plan.".to_string()),
            );
        }
    }

    pub(in crate::chatwidget) fn refresh_deferred_footer(&mut self) {
        let items = self.handoff_state.pending.as_ref().map(|pending| {
            if pending.in_flight.is_some() {
                vec![(
                    "Handoff starting".to_string(),
                    "awaiting fresh thread".to_string(),
                )]
            } else {
                vec![(
                    "Handoff ready".to_string(),
                    "next prompt starts fresh".to_string(),
                )]
            }
        });
        self.set_footer_hint_override(items);
    }

    pub(in crate::chatwidget) fn confirm_deferred_discard_if_needed(
        &mut self,
        action: DeferredDiscardAction,
    ) -> bool {
        let command = match &action {
            DeferredDiscardAction::Command(command)
            | DeferredDiscardAction::CommandWithArgs { command, .. } => *command,
        };
        if !matches!(
            command,
            SlashCommand::New
                | SlashCommand::Clear
                | SlashCommand::Fork
                | SlashCommand::Worktree
                | SlashCommand::Compact
                | SlashCommand::Handoff
                | SlashCommand::Archive
                | SlashCommand::Delete
                | SlashCommand::Quit
                | SlashCommand::Exit
                | SlashCommand::Logout
        ) {
            return false;
        }
        let Some(pending) = self.handoff_state.pending.as_ref() else {
            return false;
        };
        let source_thread_id = pending.source_thread_id;
        let generation = pending.generation;
        let command_name = command.command();
        self.show_deferred_discard_confirmation(
            format!("Running /{command_name}"),
            SelectionItem {
                name: format!("Discard plan and run /{command_name}"),
                description: Some("Continue with the selected command.".to_string()),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::ConfirmDeferredHandoffDiscard {
                        source_thread_id,
                        generation,
                        action: action.clone(),
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        );
        true
    }

    pub(crate) fn discard_deferred_handoff(
        &mut self,
        source_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        if !self.is_current_deferred_transaction(source_thread_id, generation) {
            return false;
        }
        if let Some(pending) = self.handoff_state.pending.take()
            && let Some(user_message) = pending.in_flight
        {
            self.restore_user_message_to_composer(user_message);
        }
        self.refresh_deferred_footer();
        self.add_info_message(
            "Deferred handoff plan discarded.".to_string(),
            /*hint*/ None,
        );
        true
    }

    pub(crate) fn confirm_deferred_discard_app_action(
        &mut self,
        action_label: &str,
        action: crate::bottom_pane::SelectionAction,
    ) -> bool {
        let Some(pending) = self.handoff_state.pending.as_ref() else {
            return false;
        };
        let source_thread_id = pending.source_thread_id;
        let generation = pending.generation;
        let action_label = action_label.to_string();
        self.show_deferred_discard_confirmation(
            format!("Continuing to {action_label}"),
            SelectionItem {
                name: format!("Discard plan and {action_label}"),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::DiscardDeferredHandoff {
                        source_thread_id,
                        generation,
                    });
                    action(tx);
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        );
        true
    }

    fn show_deferred_discard_confirmation(
        &mut self,
        action_subject: String,
        discard_item: SelectionItem,
    ) {
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Discard the deferred handoff plan?".to_string()),
            subtitle: Some(format!(
                "{action_subject} will discard the plan waiting for your next prompt."
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Keep the handoff plan".to_string(),
                    description: Some("Return to the current thread.".to_string()),
                    dismiss_on_select: true,
                    ..Default::default()
                },
                discard_item,
            ],
            ..SelectionViewParams::picker()
        });
        self.request_redraw();
    }
}
