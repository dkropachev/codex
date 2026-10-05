//! Manual handoff planning and source-thread UI state.

use super::*;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::chatwidget::slash_dispatch::PreparedSlashCommandArgs;
use crate::chatwidget::slash_dispatch::SlashCommandDispatchSource;
use crate::handoff::HandoffDisposition;
use crate::handoff::HandoffPlan;

#[path = "handoff_deferred.rs"]
mod deferred;

#[derive(Default)]
pub(super) struct HandoffState {
    active: Option<ManualHandoff>,
    next_generation: u64,
    pub(super) pending: Option<Box<DeferredHandoff>>,
    pub(super) recoverable_execution_prompt: Option<Box<UserMessage>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DeferredHandoff {
    pub(super) source_thread_id: ThreadId,
    pub(super) plan_turn_id: String,
    pub(super) plan_text: String,
    pub(super) generation: u64,
    pub(super) in_flight: Option<UserMessage>,
    pub(super) awaiting_default_mode_update: bool,
}

pub(super) enum DeferredSubmission {
    NotApplicable,
    Queued,
    Blocked,
}

struct ManualHandoff {
    disposition: HandoffDisposition,
    generation: u64,
    owned_turn_id: Option<String>,
    owned_submission_items: Option<Vec<UserInput>>,
    owned_turn_completed: bool,
    ready_plan_turn_id: Option<String>,
    ready_plan_text: Option<String>,
}

impl ManualHandoff {
    fn clear_turn(&mut self) {
        self.owned_turn_id = None;
        self.owned_submission_items = None;
        self.owned_turn_completed = false;
        self.ready_plan_turn_id = None;
        self.ready_plan_text = None;
    }
}

#[derive(Clone, Copy)]
enum HandoffLocalBlocker {
    ParentOwned,
    Ephemeral,
    Busy,
    PendingInteraction,
    ActiveGoal,
}

impl HandoffLocalBlocker {
    fn message(self) -> &'static str {
        match self {
            Self::ParentOwned => {
                "Another agent owns this thread's input. Return to the parent thread before using /handoff."
            }
            Self::Ephemeral => {
                "/handoff requires a resumable source thread; this session is ephemeral."
            }
            Self::Busy => "/handoff requires an idle thread with no queued input.",
            Self::PendingInteraction => {
                "/handoff is unavailable while a prompt or modal needs a response."
            }
            Self::ActiveGoal => "Finish or pause the active goal before using /handoff.",
        }
    }
}

impl ChatWidget {
    pub(super) fn handoff_mode_active(&self) -> bool {
        self.handoff_state.active.is_some() && self.active_mode_kind() == ModeKind::Plan
    }

    pub(super) fn dispatch_prepared_handoff(&mut self, prepared: PreparedSlashCommandArgs) {
        let PreparedSlashCommandArgs {
            args,
            text_elements,
            pending_pastes,
            local_images,
            remote_image_urls,
            mention_bindings,
            source,
        } = prepared;
        let (args, text_elements) = crate::bottom_pane::ChatComposer::expand_pending_pastes(
            &args,
            text_elements,
            &pending_pastes,
        );
        let parsed = match crate::handoff::parse_handoff_args(&args) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.add_error_message(error.to_string());
                return;
            }
        };
        let guidance_end = parsed.guidance_start + parsed.guidance.len();
        let prompt = crate::handoff::manual_planning_prompt(&parsed.guidance);
        let prompt_guidance_start = prompt.len().saturating_sub(parsed.guidance.len());
        let original_elements = text_elements.clone();
        let prompt_elements = text_elements
            .into_iter()
            .filter_map(|element| {
                let range = element.byte_range;
                (range.start >= parsed.guidance_start && range.end <= guidance_end).then(|| {
                    element.map_range(|range| {
                        (prompt_guidance_start + range.start - parsed.guidance_start
                            ..prompt_guidance_start + range.end - parsed.guidance_start)
                            .into()
                    })
                })
            })
            .collect();
        let mut user_message = self.prepared_inline_user_message(
            args.clone(),
            prompt_elements,
            local_images,
            remote_image_urls,
            mention_bindings,
            source,
        );
        user_message.text = prompt;

        let prefix = if args.is_empty() {
            "/handoff"
        } else {
            "/handoff "
        };
        let restore_command = |mut message: UserMessage| {
            message.text = format!("{prefix}{args}");
            message.text_elements = original_elements
                .into_iter()
                .map(|element| {
                    element.map_range(|range| {
                        (prefix.len() + range.start..prefix.len() + range.end).into()
                    })
                })
                .collect();
            message
        };
        if !self.is_session_configured() {
            self.queue_user_message_with_options(
                restore_command(user_message),
                QueuedInputAction::ParseSlash,
                pending_pastes,
            );
            return;
        }
        if !self.current_model_supports_images()
            && (!user_message.local_images.is_empty() || !user_message.remote_image_urls.is_empty())
        {
            self.add_error_message(
                "The current model cannot read handoff attachments. The command was restored."
                    .to_string(),
            );
            self.restore_user_message_to_composer(restore_command(user_message));
            return;
        }
        let original = user_message.clone();
        if !self.begin_manual_handoff(parsed.disposition, user_message) {
            self.restore_user_message_to_composer(restore_command(original));
        } else if source == SlashCommandDispatchSource::Live {
            self.bottom_pane.drain_pending_submission_state();
        }
    }

    fn handoff_local_blocker(&self) -> Option<HandoffLocalBlocker> {
        if self.blocks_direct_input || self.active_side_conversation {
            return Some(HandoffLocalBlocker::ParentOwned);
        }
        if self.config.ephemeral {
            return Some(HandoffLocalBlocker::Ephemeral);
        }
        if self.is_user_turn_pending_or_running()
            || self.bottom_pane.is_task_running()
            || !self.composer_is_empty()
            || self.has_queued_follow_up_messages()
            || !self.input_queue.pending_steers.is_empty()
            || self.input_queue.suppress_queue_autosend
            || self.input_queue.rate_limit_recovery_pending
            || self.input_queue.recovered_queue
        {
            return Some(HandoffLocalBlocker::Busy);
        }
        if !self.bottom_pane.no_modal_or_popup_active()
            || self.has_pending_protected_request()
            || matches!(
                self.rate_limit_switch_prompt,
                RateLimitSwitchPromptState::Pending
            )
        {
            return Some(HandoffLocalBlocker::PendingInteraction);
        }
        if self
            .current_goal_status
            .as_ref()
            .is_some_and(GoalStatusState::is_active)
        {
            return Some(HandoffLocalBlocker::ActiveGoal);
        }
        None
    }

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
        if !self.is_session_configured()
            || self.active_mode_kind() != ModeKind::Default
            || self.handoff_state.active.is_some()
            || self.handoff_state.pending.is_some()
        {
            self.add_error_message(
                "/handoff is available only from an idle Default-mode session.".to_string(),
            );
            return false;
        }
        if let Some(blocker) = self.handoff_local_blocker() {
            self.add_error_message(blocker.message().to_string());
            return false;
        }
        let Some(mask) = crate::handoff::handoff_mask(self.model_catalog.as_ref()) else {
            self.add_error_message("Handoff mode is unavailable right now.".to_string());
            return false;
        };

        self.handoff_state.next_generation = self.handoff_state.next_generation.wrapping_add(1);
        self.handoff_state.active = Some(ManualHandoff {
            disposition,
            generation: self.handoff_state.next_generation,
            owned_turn_id: None,
            owned_submission_items: None,
            owned_turn_completed: false,
            ready_plan_turn_id: None,
            ready_plan_text: None,
        });
        self.set_collaboration_mask_from_user_action(mask);
        let (submitted, _) = self.submit_user_message_with_history_and_shell_escape_policy(
            user_message,
            UserMessageHistoryRecord::UserMessageText,
            ShellEscapePolicy::Disallow,
            UserMessageSource::Prompt,
        );
        if !submitted {
            self.handoff_state.active = None;
            self.add_error_message(
                "Handoff planning could not start. The source thread is unchanged.".to_string(),
            );
        }
        submitted
    }

    pub(super) fn note_handoff_submission(&mut self, items: Vec<UserInput>) {
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.clear_turn();
            active.owned_submission_items = Some(items);
        }
    }

    pub(super) fn handoff_tracks_submission(&self) -> bool {
        self.handoff_state.active.is_some()
    }

    pub(crate) fn bind_handoff_turn_start(&mut self, turn_id: &str, items: &[UserInput]) {
        if let Some(active) = self.handoff_state.active.as_mut()
            && active.owned_submission_items.as_deref() == Some(items)
        {
            active.owned_submission_items = None;
            active.owned_turn_id = Some(turn_id.to_string());
        }
    }

    pub(super) fn note_handoff_turn_completed(&mut self, turn_id: &str) {
        if let Some(active) = self.handoff_state.active.as_mut()
            && active.owned_submission_items.is_none()
            && active.owned_turn_id.as_deref() == Some(turn_id)
        {
            active.owned_turn_completed = true;
        }
    }

    /// Returns true when this handoff owns the Plan-mode completion UI.
    pub(super) fn advance_manual_handoff_after_successful_turn(&mut self) -> bool {
        let Some(active) = self.handoff_state.active.as_ref() else {
            return false;
        };
        if !active.owned_turn_completed {
            return true;
        }
        let disposition = active.disposition;
        let generation = active.generation;
        let owned_turn_id = active.owned_turn_id.clone();
        let authoritative = owned_turn_id
            .as_ref()
            .filter(|turn_id| {
                self.transcript.latest_authoritative_plan_turn_id.as_ref() == Some(turn_id)
            })
            .and_then(|turn_id| {
                self.transcript
                    .latest_authoritative_plan_markdown
                    .as_ref()
                    .map(|plan| (turn_id.clone(), plan.clone()))
            });
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.clear_turn();
        }
        let Some((plan_turn_id, plan_text)) = authoritative else {
            self.add_info_message(
                "Handoff planning ended without an authoritative plan.".to_string(),
                Some(
                    "The source thread remains resumable. Continue planning in Handoff mode."
                        .to_string(),
                ),
            );
            return true;
        };
        let plan = match HandoffPlan::new(plan_text.clone()) {
            Ok(plan) => plan,
            Err(error) => {
                self.add_error_message(error.to_string());
                return true;
            }
        };
        if disposition != HandoffDisposition::Defer
            && let Some(active) = self.handoff_state.active.as_mut()
        {
            active.ready_plan_turn_id = Some(plan_turn_id.clone());
            active.ready_plan_text = Some(plan_text.clone());
        }
        match disposition {
            HandoffDisposition::Proceed => self.emit_handoff_transfer(generation, plan),
            HandoffDisposition::Ask => self.open_handoff_decision_prompt(generation, plan),
            HandoffDisposition::Defer => {
                self.defer_handoff(generation, plan_turn_id, plan_text);
            }
        }
        true
    }

    fn emit_handoff_transfer(&self, generation: u64, plan: HandoffPlan) {
        if let (Some(source_thread_id), Some(plan_turn_id)) = (
            self.thread_id,
            self.handoff_state
                .active
                .as_ref()
                .and_then(|active| active.ready_plan_turn_id.as_ref()),
        ) {
            self.app_event_tx.send(AppEvent::StartHandoffTransfer {
                source_thread_id,
                plan_turn_id: plan_turn_id.clone(),
                generation,
                plan: plan.into_text(),
            });
        }
    }

    fn open_handoff_decision_prompt(&mut self, generation: u64, plan: HandoffPlan) {
        let Some(source_thread_id) = self.thread_id else {
            return;
        };
        let Some(plan_turn_id) = self
            .handoff_state
            .active
            .as_ref()
            .and_then(|active| active.ready_plan_turn_id.clone())
        else {
            return;
        };
        let plan_text = plan.into_text();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Handoff plan ready".to_string()),
            subtitle: Some("Choose how to continue with the validated plan.".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![
                SelectionItem {
                    name: "Start fresh and proceed".to_string(),
                    description: Some("Execute the plan in a fresh thread now.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::StartHandoffTransfer {
                            source_thread_id,
                            plan_turn_id: plan_turn_id.clone(),
                            generation,
                            plan: plan_text.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Stay in Handoff mode".to_string(),
                    description: Some("Keep refining the plan in this thread.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::StayInHandoff {
                            source_thread_id,
                            generation,
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
                });
            })),
            ..Default::default()
        });
        self.request_redraw();
    }

    pub(crate) fn stay_in_handoff(&mut self, generation: u64) {
        if let Some(active) = self.handoff_state.active.as_mut()
            && active.generation == generation
        {
            active.clear_turn();
            self.handoff_state.next_generation = self.handoff_state.next_generation.wrapping_add(1);
            active.generation = self.handoff_state.next_generation;
        }
    }

    pub(crate) fn is_current_handoff_transaction(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        self.thread_id == Some(source_thread_id)
            && self
                .handoff_state
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
    }

    pub(crate) fn handoff_transfer_is_locally_safe(
        &self,
        source_thread_id: ThreadId,
        generation: u64,
        plan: &str,
    ) -> bool {
        let Some(active) = self.handoff_state.active.as_ref() else {
            return false;
        };
        self.thread_id == Some(source_thread_id)
            && active.generation == generation
            && self.handoff_mode_active()
            && active.ready_plan_turn_id.as_ref()
                == self.transcript.latest_authoritative_plan_turn_id.as_ref()
            && active.ready_plan_text.as_ref()
                == self.transcript.latest_authoritative_plan_markdown.as_ref()
            && active.ready_plan_text.as_deref() == Some(plan)
            && HandoffPlan::new(plan.to_string()).is_ok()
            && self.handoff_local_blocker().is_none()
    }

    pub(super) fn leave_handoff_for_user_mode_change(&mut self, mask: &CollaborationModeMask) {
        if !crate::handoff::is_handoff_mask(Some(mask)) {
            self.handoff_state.active = None;
        }
    }

    pub(super) fn stop_handoff_after_turn_failure(&mut self) {
        if let Some(active) = self.handoff_state.active.as_mut() {
            active.clear_turn();
        }
    }

    pub(super) fn fail_handoff_mode_update(&mut self) {
        if self.handoff_state.active.take().is_none() {
            return;
        }
        if let Some(default_mask) =
            collaboration_modes::default_mode_mask(self.model_catalog.as_ref())
        {
            self.set_collaboration_mask(default_mask);
        }
        self.add_error_message(
            "Handoff mode could not be applied. The source thread is unchanged; retry /handoff when collaboration settings are available."
                .to_string(),
        );
    }
}
