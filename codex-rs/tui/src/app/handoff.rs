//! Fresh-session transfer and app-level safety gates for plan handoff.

use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::handoff::HandoffDisposition;
use crate::handoff::HandoffTelemetryDisposition;
use crate::handoff::HandoffTelemetryEvent;
use crate::handoff::HandoffTelemetryReason;
use crate::handoff::HandoffTrigger;
use crate::handoff::PendingHandoffPlan;

#[derive(Clone)]
struct PreservedHandoffSettings {
    model: String,
    reasoning_effort: Option<ReasoningEffortConfig>,
    service_tier: Option<String>,
}

impl App {
    async fn start_fresh_handoff_session_with_summary_hint(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        let preserved = PreservedHandoffSettings {
            model: self.chat_widget.current_model().to_string(),
            reasoning_effort: self
                .chat_widget
                .current_collaboration_mode()
                .reasoning_effort(),
            service_tier: self.chat_widget.current_service_tier().map(str::to_string),
        };
        self.refresh_in_memory_config_from_disk_best_effort("starting a handoff thread")
            .await;
        let mut config = self.fresh_session_config();
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
        let started = app_server
            .start_thread_with_session_start_source(
                &config,
                Some(ThreadStartSource::Clear),
                /*remote_cwd_override*/ None,
            )
            .await?;

        self.clear_terminal_ui(tui, /*redraw_header*/ false)?;
        self.reset_app_ui_state_after_clear();
        self.shutdown_current_thread(app_server).await;
        let tracked_thread_ids: Vec<ThreadId> =
            self.thread_event_channels.keys().copied().collect();
        for thread_id in tracked_thread_ids {
            if let Err(error) = app_server.thread_unsubscribe(thread_id).await {
                tracing::warn!("failed to unsubscribe tracked thread {thread_id}: {error}");
            }
        }
        self.config = config;
        self.replace_chat_widget_with_app_server_thread(
            tui,
            started,
            ThreadAttachPresentation::SessionLineage,
            /*initial_user_message*/ None,
        )
        .await?;
        if let Some(summary) = summary {
            let mut lines: Vec<Line<'static>> = Vec::new();
            if let Some(usage_line) = summary.usage_line {
                lines.push(usage_line.into());
            }
            if let Some(command) = summary.resume_hint {
                lines.push(vec!["To continue this session, run ".into(), command.cyan()].into());
            }
            self.chat_widget.add_plain_history_lines(lines);
        }
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start_handoff_transfer(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        source_thread_id: ThreadId,
        generation: u64,
        plan: String,
        disposition: HandoffDisposition,
        trigger: HandoffTrigger,
    ) -> Result<()> {
        if self.current_displayed_thread_id() != Some(source_thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(source_thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || !self.chat_widget.handoff_transfer_is_locally_safe(
                source_thread_id,
                generation,
                trigger,
                disposition,
            )
        {
            if self
                .chat_widget
                .is_current_handoff_transaction(source_thread_id, generation)
            {
                self.chat_widget.add_info_message(
                    "Handoff transfer paused because the source thread is no longer safely idle."
                        .to_string(),
                    Some("The source thread was not cleared; finish the pending interaction and retry in Handoff mode.".to_string()),
                );
                self.chat_widget
                    .resume_handoff_after_transfer_failure(trigger, disposition);
                self.record_handoff_transfer_failure(
                    trigger,
                    HandoffTelemetryReason::SourceChanged,
                );
            }
            return Ok(());
        }
        let pending = match PendingHandoffPlan::new(plan) {
            Ok(pending) => pending,
            Err(error) => {
                self.chat_widget.add_error_message(error.to_string());
                self.chat_widget
                    .resume_handoff_after_transfer_failure(trigger, disposition);
                self.record_handoff_transfer_failure(trigger, HandoffTelemetryReason::InvalidPlan);
                return Ok(());
            }
        };
        if disposition == HandoffDisposition::Ask {
            self.chat_widget.add_error_message(
                "Internal handoff error: unresolved ask disposition.".to_string(),
            );
            self.record_handoff_transfer_failure(
                trigger,
                HandoffTelemetryReason::UnresolvedDisposition,
            );
            return Ok(());
        }
        HandoffTelemetryEvent::Disposition {
            trigger,
            disposition: HandoffTelemetryDisposition::from(disposition),
        }
        .record(&self.session_telemetry);

        if self.has_active_handoff_descendant(source_thread_id).await {
            self.chat_widget.add_error_message(
                "Handoff transfer was cancelled because descendant agents are still active. Finish them, then request a new plan in Handoff mode."
                    .to_string(),
            );
            self.chat_widget
                .resume_handoff_after_transfer_failure(trigger, disposition);
            self.record_handoff_transfer_failure(
                trigger,
                HandoffTelemetryReason::ActiveDescendants,
            );
            return Ok(());
        }
        if let Err(error) = self
            .start_fresh_handoff_session_with_summary_hint(tui, app_server)
            .await
        {
            self.chat_widget.add_error_message(format!(
                "Failed to finish the fresh handoff transfer: {error}. The source thread remains resumable."
            ));
            self.chat_widget
                .resume_handoff_after_transfer_failure(trigger, disposition);
            self.record_handoff_transfer_failure(trigger, HandoffTelemetryReason::ThreadStart);
            return Ok(());
        }
        let Some(destination_thread_id) = self.chat_widget.thread_id() else {
            self.chat_widget.add_error_message(
                "Failed to start the fresh handoff thread. The source thread remains resumable."
                    .to_string(),
            );
            self.record_handoff_transfer_failure(trigger, HandoffTelemetryReason::ThreadStart);
            return Ok(());
        };
        if destination_thread_id == source_thread_id {
            self.chat_widget.add_error_message(
                "Failed to start the fresh handoff thread. The source thread remains resumable."
                    .to_string(),
            );
            self.record_handoff_transfer_failure(trigger, HandoffTelemetryReason::ThreadStart);
            return Ok(());
        }

        match disposition {
            HandoffDisposition::Proceed => {
                let submitted = self.chat_widget.submit_handoff_execution(pending, trigger);
                let Some(pending) = self.chat_widget.pending_handoff_state().cloned() else {
                    self.record_handoff_transfer_failure(
                        trigger,
                        HandoffTelemetryReason::ExecutionSubmission,
                    );
                    return Ok(());
                };
                self.pending_handoffs.insert(destination_thread_id, pending);
                if !submitted {
                    self.chat_widget.redisplay_pending_handoff();
                    self.record_handoff_transfer_failure(
                        trigger,
                        HandoffTelemetryReason::ExecutionSubmission,
                    );
                    return Ok(());
                }
            }
            HandoffDisposition::Defer => {
                self.chat_widget.install_pending_handoff(pending);
                let Some(pending) = self.chat_widget.pending_handoff_state().cloned() else {
                    self.record_handoff_transfer_failure(
                        trigger,
                        HandoffTelemetryReason::ExecutionSubmission,
                    );
                    return Ok(());
                };
                self.pending_handoffs.insert(destination_thread_id, pending);
            }
            HandoffDisposition::Ask => unreachable!("ask disposition was rejected above"),
        }

        if disposition == HandoffDisposition::Defer {
            HandoffTelemetryEvent::Completion {
                trigger,
                disposition,
            }
            .record(&self.session_telemetry);
        }
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
        if !self.chat_widget.automatic_handoff_is_locally_eligible() {
            return;
        }
        self.chat_widget.start_automatic_handoff();
    }

    pub(super) async fn maybe_advance_automatic_handoff_planning(
        &mut self,
        source_thread_id: ThreadId,
        generation: u64,
    ) {
        let blocked = self.primary_thread_id != Some(source_thread_id)
            || self.current_displayed_thread_id() != Some(source_thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(source_thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || self.has_active_handoff_descendant(source_thread_id).await;
        if blocked {
            if self.chat_widget.thread_id() == Some(source_thread_id) {
                self.chat_widget
                    .cancel_automatic_handoff_at_app_gate(generation);
            }
            return;
        }
        self.chat_widget
            .continue_automatic_handoff_planning(generation);
    }

    async fn has_active_handoff_descendant(&self, thread_id: ThreadId) -> bool {
        if self
            .agent_navigation
            .has_running_thread_other_than(thread_id)
        {
            return true;
        }
        for (candidate, channel) in &self.thread_event_channels {
            if *candidate == thread_id
                || self
                    .agent_navigation
                    .get(candidate)
                    .is_some_and(|entry| entry.is_closed)
            {
                continue;
            }
            if channel.store.lock().await.active_turn_id().is_some() {
                return true;
            }
        }
        false
    }

    pub(super) fn discard_pending_handoff_for_current_thread(&mut self) {
        if let Some(thread_id) = self.chat_widget.thread_id() {
            self.pending_handoffs.remove(&thread_id);
        }
        self.chat_widget.discard_pending_handoff();
    }

    fn record_handoff_transfer_failure(
        &self,
        trigger: HandoffTrigger,
        reason: HandoffTelemetryReason,
    ) {
        HandoffTelemetryEvent::Failure { trigger, reason }.record(&self.session_telemetry);
    }
}
