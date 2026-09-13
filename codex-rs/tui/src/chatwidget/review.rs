//! Code-review flow state for `ChatWidget`.

use std::path::Path;
use std::path::PathBuf;

use codex_app_server_protocol::ReviewTarget;
use codex_app_server_protocol::TurnStatus;
use codex_protocol::ThreadId;
use uuid::Uuid;

use super::ChatWidget;
use crate::app_command::AppCommand;
use crate::auto_review_denials::RecentAutoReviewDenials;
use crate::review_scope::ReviewScopeResolution;
use crate::token_usage::TokenUsageInfo;

const FIX_FINDINGS_PROMPT: &str = concat!(
    "Revalidate every finding from the code review against the current code, its callers, tests, ",
    "and intended behavior. Fix every finding that is still valid, run the relevant tests, and ",
    "report any findings you rejected and why."
);
const FIX_AND_COMMIT_FINDINGS_PROMPT: &str = concat!(
    "Revalidate every finding from the code review against the current code, its callers, tests, ",
    "and intended behavior. Fix every finding that is still valid and run the relevant tests. ",
    "Report any findings you rejected and why. Then create one focused new commit containing only ",
    "the review fixes. Preserve unrelated working-tree changes, do not amend an existing commit, ",
    "and do not push."
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewAction {
    Report,
    Fix,
    FixAndCommit,
}

impl ReviewAction {
    fn follow_up_prompt(self) -> Option<&'static str> {
        match self {
            Self::Report => None,
            Self::Fix => Some(FIX_FINDINGS_PROMPT),
            Self::FixAndCommit => Some(FIX_AND_COMMIT_FINDINGS_PROMPT),
        }
    }
}

#[derive(Debug)]
struct PendingScopeResolution {
    request_id: Uuid,
    cwd: PathBuf,
}

#[derive(Debug)]
struct ActiveReview {
    thread_id: ThreadId,
    turn_id: String,
    action: ReviewAction,
    finding_count: Option<usize>,
}

#[derive(Debug)]
struct SelectedReviewAction {
    thread_id: ThreadId,
    action: ReviewAction,
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
    selected_action: Option<SelectedReviewAction>,
    active_review: Option<ActiveReview>,
    ready_follow_up: Option<ReviewAction>,
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

    pub(super) fn stage_action(
        &mut self,
        thread_id: Option<ThreadId>,
        action: ReviewAction,
    ) -> bool {
        if self.selected_action.is_some()
            || self.active_review.is_some()
            || self.ready_follow_up.is_some()
        {
            return false;
        }
        let Some(thread_id) = thread_id else {
            return false;
        };
        self.selected_action = Some(SelectedReviewAction { thread_id, action });
        true
    }

    pub(super) fn bind_live_review(&mut self, thread_id: &str, turn_id: String) {
        if self.ready_follow_up.is_some() {
            return;
        }
        let Ok(thread_id) = ThreadId::from_string(thread_id) else {
            self.clear_action();
            return;
        };
        if self
            .active_review
            .as_ref()
            .is_some_and(|active| active.thread_id == thread_id && active.turn_id == turn_id)
        {
            return;
        }
        let Some(selected) = self.selected_action.take() else {
            self.clear_action();
            return;
        };
        if selected.thread_id != thread_id {
            self.clear_action();
            return;
        }
        self.active_review = Some(ActiveReview {
            thread_id,
            turn_id,
            action: selected.action,
            finding_count: None,
        });
    }

    pub(super) fn note_live_review_exit(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        finding_count: usize,
    ) {
        let thread_id = ThreadId::from_string(thread_id).ok();
        let matches_active = self
            .active_review
            .as_ref()
            .is_some_and(|active| Some(active.thread_id) == thread_id && active.turn_id == turn_id);
        if !matches_active {
            self.clear_action();
            return;
        }
        if let Some(active) = self.active_review.as_mut() {
            active.finding_count = Some(finding_count);
        }
    }

    pub(super) fn note_turn_terminal(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        status: &TurnStatus,
        from_replay: bool,
    ) {
        if from_replay {
            self.clear_action();
            return;
        }

        let thread_id = ThreadId::from_string(thread_id).ok();
        match status {
            TurnStatus::Completed => {
                let Some(active) = self.active_review.take() else {
                    if self.selected_action.is_some() {
                        self.clear_action();
                    }
                    return;
                };
                if Some(active.thread_id) != thread_id || active.turn_id != turn_id {
                    self.clear_action();
                    return;
                }
                self.selected_action = None;
                self.ready_follow_up = active
                    .finding_count
                    .is_some_and(|finding_count| finding_count > 0)
                    .then_some(active.action)
                    .filter(|action| action.follow_up_prompt().is_some());
            }
            TurnStatus::Interrupted | TurnStatus::Failed => self.clear_action(),
            TurnStatus::InProgress => {}
        }
    }

    pub(super) fn take_ready_follow_up(&mut self) -> Option<ReviewAction> {
        self.ready_follow_up.take()
    }

    pub(super) fn has_ready_follow_up(&self) -> bool {
        self.ready_follow_up.is_some()
    }

    pub(super) fn clear_action(&mut self) {
        self.selected_action = None;
        self.active_review = None;
        self.ready_follow_up = None;
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
        action: ReviewAction,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        if self.is_user_turn_pending_or_running() {
            self.add_error_message(
                "Wait for the current turn to finish before starting a review.".to_string(),
            );
            return;
        }
        if !self.review.stage_action(thread_id, action) {
            self.add_error_message(
                "Finish the current review action before starting another review.".to_string(),
            );
            return;
        }
        if !self.submit_op(AppCommand::review(target)) {
            self.review.clear_action();
        }
    }

    pub(crate) fn bind_live_review_action(&mut self, thread_id: &str, turn_id: String) {
        self.review.bind_live_review(thread_id, turn_id);
    }

    pub(super) fn note_live_review_exit(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        finding_count: usize,
    ) {
        self.review
            .note_live_review_exit(thread_id, turn_id, finding_count);
    }

    pub(super) fn note_review_turn_terminal(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        status: &TurnStatus,
        from_replay: bool,
    ) {
        self.review
            .note_turn_terminal(thread_id, turn_id, status, from_replay);
    }

    pub(crate) fn clear_review_action(&mut self) {
        self.review.clear_action();
    }

    pub(super) fn maybe_submit_ready_review_follow_up(&mut self) -> bool {
        if self.is_user_turn_pending_or_running() {
            return false;
        }
        let Some(action) = self.review.take_ready_follow_up() else {
            return false;
        };
        let Some(prompt) = action.follow_up_prompt() else {
            return false;
        };
        let Some(default_mode) =
            crate::collaboration_modes::default_mode_mask(self.model_catalog.as_ref())
        else {
            self.add_error_message(
                "Could not fix review findings because Default mode is unavailable.".to_string(),
            );
            return false;
        };

        self.submit_user_message_with_mode(prompt.to_string(), default_mode);
        self.is_user_turn_pending_or_running()
    }
}
