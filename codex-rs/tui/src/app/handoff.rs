//! Fresh-thread transfer and app-owned safety gates for manual handoff.

use super::session_lifecycle::FreshThreadTransition;
use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::chatwidget::UserMessage;
use crate::handoff::HandoffPlan;

pub(super) enum HandoffTransferKind {
    Proceed,
    Deferred { user_message: UserMessage },
}

pub(super) struct HandoffTransferRequest {
    pub(super) source_thread_id: ThreadId,
    pub(super) plan_turn_id: String,
    pub(super) generation: u64,
    pub(super) plan: String,
    pub(super) kind: HandoffTransferKind,
}

impl HandoffTransferKind {
    fn is_current_transaction(
        &self,
        chat_widget: &ChatWidget,
        source_thread_id: ThreadId,
        generation: u64,
    ) -> bool {
        match self {
            Self::Proceed => {
                chat_widget.is_current_handoff_transaction(source_thread_id, generation)
            }
            Self::Deferred { .. } => {
                chat_widget.is_current_deferred_transaction(source_thread_id, generation)
            }
        }
    }

    fn is_locally_safe(
        &self,
        chat_widget: &ChatWidget,
        source_thread_id: ThreadId,
        generation: u64,
        plan: &str,
    ) -> bool {
        match self {
            Self::Proceed => {
                chat_widget.handoff_transfer_is_locally_safe(source_thread_id, generation, plan)
            }
            Self::Deferred { user_message } => chat_widget.deferred_transfer_is_locally_safe(
                source_thread_id,
                generation,
                plan,
                user_message,
            ),
        }
    }

    fn rollback(&self, chat_widget: &mut ChatWidget, generation: u64) {
        match self {
            Self::Proceed => chat_widget.stay_in_handoff(generation),
            Self::Deferred { .. } => chat_widget.rollback_deferred_handoff(generation),
        }
    }

    fn initial_user_message(
        &self,
        plan: &HandoffPlan,
    ) -> Result<UserMessage, crate::handoff::HandoffPlanValidationError> {
        match self {
            Self::Proceed => Ok(plan.execution_prompt().into()),
            Self::Deferred { user_message } => {
                let text = plan.execution_prompt_with_followup(&user_message.text)?;
                let user_text_start = text.len() - user_message.text.len();
                let mut merged = user_message.clone();
                merged.text_elements = merged
                    .text_elements
                    .into_iter()
                    .map(|element| {
                        element.map_range(|range| {
                            (user_text_start + range.start..user_text_start + range.end).into()
                        })
                    })
                    .collect();
                merged.text = text;
                Ok(merged)
            }
        }
    }
}

struct SourceRevision {
    metadata: codex_app_server_protocol::Thread,
    latest_turn: Turn,
    event_latest_turn_id: Option<String>,
    event_active_turn_id: Option<String>,
}

impl SourceRevision {
    fn matches_plan_turn(&self, plan_turn_id: &str) -> bool {
        matches!(
            self.metadata.status,
            codex_app_server_protocol::ThreadStatus::Idle
        ) && self.event_active_turn_id.is_none()
            && self.event_latest_turn_id.as_deref() == Some(plan_turn_id)
            && self.latest_turn.id == plan_turn_id
            && self.latest_turn.status == TurnStatus::Completed
    }
}

struct PreservedHandoffSettings {
    model: String,
    reasoning_effort: Option<ReasoningEffortConfig>,
    service_tier: Option<String>,
}

impl App {
    async fn read_handoff_source_revision(
        &self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<SourceRevision> {
        let metadata = app_server
            .thread_read(thread_id, /*include_turns*/ false)
            .await?;
        let latest_turn = app_server
            .thread_turns_page(thread_id, /*cursor*/ None, /*limit*/ 1)
            .await?
            .data
            .into_iter()
            .next()
            .ok_or_else(|| color_eyre::eyre::eyre!("source thread has no persisted Plan turn"))?;
        let (event_latest_turn_id, event_active_turn_id) =
            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let store = channel.store.lock().await;
                (
                    store.latest_turn_id.clone(),
                    store.active_turn_id().map(str::to_string),
                )
            } else {
                (None, None)
            };
        Ok(SourceRevision {
            metadata,
            latest_turn,
            event_latest_turn_id,
            event_active_turn_id,
        })
    }

    pub(super) async fn start_handoff_transfer(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        request: HandoffTransferRequest,
    ) -> Result<()> {
        let HandoffTransferRequest {
            source_thread_id,
            plan_turn_id,
            generation,
            plan,
            kind,
        } = request;
        if self.current_displayed_thread_id() != Some(source_thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(source_thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || self.has_active_handoff_descendant(source_thread_id).await
            || !kind.is_locally_safe(&self.chat_widget, source_thread_id, generation, &plan)
            || self.reject_pending_permission_root_switch()
        {
            if kind.is_current_transaction(&self.chat_widget, source_thread_id, generation) {
                self.chat_widget.add_info_message(
                    "Handoff transfer paused because the source thread is no longer safely idle."
                        .to_string(),
                    Some("The source thread remains resumable. Finish the pending interaction and retry in Handoff mode.".to_string()),
                );
                kind.rollback(&mut self.chat_widget, generation);
            }
            return Ok(());
        }
        let plan = match HandoffPlan::new(plan) {
            Ok(plan) => plan,
            Err(error) => {
                self.chat_widget.add_error_message(error.to_string());
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
        };
        let initial_user_message = match kind.initial_user_message(&plan) {
            Ok(message) => message,
            Err(error) => {
                self.chat_widget.add_error_message(error.to_string());
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
        };
        let recoverable_execution_prompt = matches!(&kind, HandoffTransferKind::Deferred { .. })
            .then(|| initial_user_message.clone());
        let source_revision = match self
            .read_handoff_source_revision(app_server, source_thread_id)
            .await
        {
            Ok(revision) if revision.matches_plan_turn(&plan_turn_id) => revision,
            _ => {
                self.chat_widget.add_info_message(
                    "Handoff transfer paused because the source Plan could not be verified."
                        .to_string(),
                    Some("The source thread remains resumable. Review its latest input or use a server that can read the latest turn before retrying.".to_string()),
                );
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
        };
        let preserved = PreservedHandoffSettings {
            model: self.chat_widget.current_model().to_string(),
            reasoning_effort: self
                .chat_widget
                .current_collaboration_mode()
                .reasoning_effort(),
            service_tier: self.chat_widget.current_service_tier().map(str::to_string),
        };
        let mut config = match self.load_new_session_config(app_server).await {
            Ok(config) => config,
            Err(error) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to prepare the fresh handoff thread: {error}. The source thread remains resumable."
                ));
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
        };
        apply_managed_new_thread_defaults(
            &mut config,
            app_server.managed_new_thread_defaults(),
            &self.cli_kv_overrides,
            &self.harness_overrides,
        );
        config.model = Some(preserved.model);
        config.model_reasoning_effort = preserved.reasoning_effort;
        config.service_tier = preserved.service_tier;
        let summary = session_summary(
            self.chat_widget.token_usage(),
            self.chat_widget.thread_id(),
            self.chat_widget.thread_name(),
            self.chat_widget.rollout_path().as_deref(),
        );
        let started = match app_server
            .start_thread_with_session_start_source(
                &self.local_settings,
                &config,
                Some(ThreadStartSource::Clear),
                /*remote_cwd_override*/ None,
                /*selected_profile*/ None,
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                self.chat_widget.add_error_message(format!(
                    "Failed to start the fresh handoff thread: {error}. The source thread remains resumable."
                ));
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
        };

        // The source may have received input through another client while the two app-server
        // requests above were pending. Check its latest persisted turn before leaving it.
        // The server does not expose an atomic cross-thread handoff; another client can still
        // change the source after this read, so this remains a best-effort transfer gate.
        let source_is_current = self
            .read_handoff_source_revision(app_server, source_thread_id)
            .await
            .is_ok_and(|revision| {
                revision.matches_plan_turn(&plan_turn_id)
                    && revision.latest_turn == source_revision.latest_turn
                    && revision.event_latest_turn_id == source_revision.event_latest_turn_id
            });
        if !source_is_current {
            self.chat_widget.add_info_message(
                "Handoff transfer paused because the source Plan could not be verified after preparation."
                    .to_string(),
                Some("The source thread remains resumable. Review its latest input or use a server that can read the latest turn before retrying.".to_string()),
            );
            kind.rollback(&mut self.chat_widget, generation);
            return Ok(());
        }

        if let Err(error) = self
            .finish_fresh_thread_transition(
                tui,
                app_server,
                FreshThreadTransition {
                    started,
                    config,
                    initial_user_message: Some(initial_user_message),
                    presentation: ThreadAttachPresentation::SessionLineage,
                    summary,
                },
            )
            .await
        {
            if self.current_displayed_thread_id() == Some(source_thread_id)
                && self.active_thread_id == Some(source_thread_id)
            {
                self.chat_widget.add_error_message(format!(
                    "Failed to attach the fresh handoff thread: {error}. The source thread remains resumable."
                ));
                kind.rollback(&mut self.chat_widget, generation);
                return Ok(());
            }
            return Err(error).wrap_err(
                "Failed to attach the fresh handoff thread after leaving the source; resume the source thread from its saved rollout",
            );
        }
        if let Some(user_message) = recoverable_execution_prompt {
            self.chat_widget
                .mark_deferred_execution_prompt(user_message);
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(super) async fn maybe_start_automatic_handoff(&mut self, thread_id: ThreadId) {
        if self.primary_thread_id != Some(thread_id)
            || self.current_displayed_thread_id() != Some(thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || self.has_active_handoff_descendant(thread_id).await
        {
            return;
        }
        self.chat_widget.start_automatic_handoff();
    }

    pub(super) async fn maybe_advance_automatic_handoff_planning(
        &mut self,
        source_thread_id: ThreadId,
        generation: u64,
    ) {
        if self.primary_thread_id != Some(source_thread_id)
            || self.current_displayed_thread_id() != Some(source_thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(source_thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || self.has_active_handoff_descendant(source_thread_id).await
        {
            if self.chat_widget.thread_id() == Some(source_thread_id) {
                self.chat_widget
                    .cancel_automatic_handoff_at_app_gate(generation);
            }
            return;
        }
        self.chat_widget
            .continue_automatic_handoff_planning(generation);
    }

    async fn has_active_handoff_descendant(&self, source_thread_id: ThreadId) -> bool {
        if self
            .agent_navigation
            .ordered_threads()
            .into_iter()
            .any(|(thread_id, entry)| {
                thread_id != source_thread_id && entry.is_running && !entry.is_closed
            })
        {
            return true;
        }
        for (thread_id, channel) in &self.thread_event_channels {
            if *thread_id != source_thread_id
                && channel.store.lock().await.active_turn_id().is_some()
            {
                return true;
            }
        }
        false
    }
}
