//! Code-review flow state for `ChatWidget`.

use std::path::Path;
use std::path::PathBuf;

use codex_app_server_protocol::ReviewTarget;
use uuid::Uuid;

use super::ChatWidget;
use crate::app_command::AppCommand;
use crate::review_scope::ReviewScopeResolution;
use crate::token_usage::TokenUsageInfo;

#[derive(Debug)]
struct PendingScopeResolution {
    request_id: Uuid,
    cwd: PathBuf,
}

#[derive(Debug, Default)]
pub(super) struct ReviewState {
    /// Simple review mode flag; used to adjust layout and banners.
    pub(super) is_review_mode: bool,
    /// Snapshot of token usage to restore after review mode exits.
    pub(super) pre_review_token_info: Option<Option<TokenUsageInfo>>,
    pending_scope_resolution: Option<PendingScopeResolution>,
    resolved_scope: Option<(PathBuf, ReviewScopeResolution)>,
    pub(super) pending_review: bool,
}

impl ReviewState {
    pub(super) fn begin_scope_resolution(&mut self, cwd: PathBuf) -> Uuid {
        let request_id = Uuid::new_v4();
        self.pending_scope_resolution = Some(PendingScopeResolution { request_id, cwd });
        self.resolved_scope = None;
        request_id
    }

    pub(super) fn scope_resolution_matches(&self, request_id: Uuid, cwd: &Path) -> bool {
        self.pending_scope_resolution
            .as_ref()
            .is_some_and(|pending| pending.request_id == request_id && pending.cwd.as_path() == cwd)
    }

    pub(super) fn set_scope_resolution(
        &mut self,
        request_id: Uuid,
        cwd: PathBuf,
        resolution: ReviewScopeResolution,
    ) {
        if !self.scope_resolution_matches(request_id, &cwd) {
            return;
        }
        self.pending_scope_resolution = None;
        self.resolved_scope = Some((cwd, resolution));
    }

    pub(super) fn resolved_scope(&self, cwd: &Path) -> Option<&ReviewScopeResolution> {
        self.resolved_scope
            .as_ref()
            .filter(|(resolved_cwd, _)| resolved_cwd == cwd)
            .map(|(_, resolution)| resolution)
    }

    pub(super) fn reset_for_thread_change(&mut self) {
        *self = Self::default();
    }
}

impl ChatWidget {
    pub(super) fn enter_review_mode_with_hint(&mut self, hint: String, from_replay: bool) {
        self.review.pending_review = false;
        if self.review.pre_review_token_info.is_none() {
            self.review.pre_review_token_info = Some(self.token_info.clone());
        }
        if !from_replay && !self.bottom_pane.is_task_running() {
            self.bottom_pane.set_task_running(/*running*/ true);
        }
        self.review.is_review_mode = true;
        self.add_to_history(crate::history_cell::new_review_status_line(format!(
            ">> Code review started: {hint} <<"
        )));
        self.request_redraw();
    }

    pub(super) fn exit_review_mode_after_item(&mut self) {
        self.flush_answer_stream_with_separator();
        self.flush_interrupt_queue();
        self.flush_active_cell();
        self.review.is_review_mode = false;
        if let Some(saved) = self.review.pre_review_token_info.take() {
            match saved {
                Some(info) => {
                    self.apply_token_info(info, super::context_pressure::UsageUpdate::Uncorrelated)
                }
                None => {
                    self.bottom_pane
                        .set_context_window(/*percent*/ None, /*used_tokens*/ None);
                    self.token_info = None;
                }
            }
        }
        self.add_to_history(crate::history_cell::new_review_status_line(
            "<< Code review finished >>".to_string(),
        ));
        self.request_redraw();
    }

    pub(crate) fn start_review_for_thread(
        &mut self,
        thread_id: Option<codex_protocol::ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
    ) {
        if thread_id.is_none() || self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        if self.is_user_turn_pending_or_running() {
            self.add_error_message(
                "Wait for the current turn to finish before starting a review.".to_string(),
            );
            return;
        }
        self.review.pending_review = self.submit_op(AppCommand::review(target));
    }

    pub(crate) fn clear_pending_review(&mut self) {
        self.review.pending_review = false;
    }
}
