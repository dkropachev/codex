//! Fresh-thread transfer and app-owned safety gates for manual handoff.

use super::session_lifecycle::ThreadAttachPresentation;
use super::*;
use crate::handoff::HandoffPlan;

struct PreservedHandoffSettings {
    model: String,
    reasoning_effort: Option<ReasoningEffortConfig>,
    service_tier: Option<String>,
}

impl App {
    pub(super) async fn start_handoff_transfer(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        source_thread_id: ThreadId,
        generation: u64,
        plan: String,
    ) -> Result<()> {
        if self.current_displayed_thread_id() != Some(source_thread_id)
            || self.overlay.is_some()
            || self.agent_navigation.is_parent_owned(source_thread_id)
            || self.pending_app_server_requests.has_pending_user_input()
            || self.has_active_handoff_descendant(source_thread_id).await
            || !self.chat_widget.handoff_transfer_is_locally_safe(
                source_thread_id,
                generation,
                &plan,
            )
            || self.reject_pending_permission_root_switch()
        {
            if self
                .chat_widget
                .is_current_handoff_transaction(source_thread_id, generation)
            {
                self.chat_widget.add_info_message(
                    "Handoff transfer paused because the source thread is no longer safely idle."
                        .to_string(),
                    Some("The source thread remains resumable. Finish the pending interaction and retry in Handoff mode.".to_string()),
                );
                self.chat_widget.stay_in_handoff(generation);
            }
            return Ok(());
        }
        let plan = match HandoffPlan::new(plan) {
            Ok(plan) => plan,
            Err(error) => {
                self.chat_widget.add_error_message(error.to_string());
                self.chat_widget.stay_in_handoff(generation);
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
                self.chat_widget.stay_in_handoff(generation);
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
                self.chat_widget.stay_in_handoff(generation);
                return Ok(());
            }
        };

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
        self.local_settings = self.local_settings.reloaded(&config);
        self.config = config;
        if let Err(error) = self
            .replace_chat_widget_with_app_server_thread(
                tui,
                started,
                ThreadAttachPresentation::SessionLineage,
                Some(plan.execution_prompt().into()),
            )
            .await
        {
            self.chat_widget.add_error_message(format!(
                "Failed to attach the fresh handoff thread: {error}. The source thread remains resumable."
            ));
            return Ok(());
        }
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
