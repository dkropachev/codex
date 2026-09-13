//! Code-review flow state for `ChatWidget`.

use std::path::Path;
use std::path::PathBuf;

use codex_app_server_protocol::ReviewAction;
use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::ReviewVerification;
use codex_protocol::ThreadId;
use uuid::Uuid;

use super::ChatWidget;
use crate::app_command::AppCommand;
use crate::auto_review_denials::RecentAutoReviewDenials;
use crate::review_scope::ReviewScopeResolution;
use crate::token_usage::TokenUsageInfo;

#[derive(Debug)]
struct PendingScopeResolution {
    request_id: Uuid,
    cwd: PathBuf,
}

#[derive(Debug, Default)]
pub(super) struct ReviewState {
    pub(super) recent_auto_review_denials: RecentAutoReviewDenials,
    /// Simple review mode flag; used to adjust layout and banners.
    pub(super) is_review_mode: bool,
    /// Snapshot of token usage to restore after review mode exits.
    pub(super) pre_review_token_info: Option<Option<TokenUsageInfo>>,
    pending_scope_resolution: Option<PendingScopeResolution>,
    resolved_scope: Option<(PathBuf, ReviewScopeResolution)>,
    pub(super) legacy_report_dedupe: Option<String>,
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
    pub(crate) fn start_review_for_thread(
        &mut self,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
        verification: ReviewVerification,
        action: ReviewAction,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        if thread_id.is_none() {
            self.add_error_message("No active thread is available for review.".to_string());
            return;
        }
        if self.is_user_turn_pending_or_running() {
            self.add_error_message(
                "Wait for the current turn to finish before starting a review.".to_string(),
            );
            return;
        }
        self.submit_op(AppCommand::review(target, verification, action));
    }
}
