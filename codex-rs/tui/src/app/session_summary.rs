//! Summary shown when a fresh thread replaces the current one.

use super::resumable_thread;
use crate::token_usage::TokenUsage;
use codex_protocol::ThreadId;
use std::path::Path;

pub(super) struct SessionSummary {
    pub(super) usage_line: Option<String>,
    pub(super) resume_hint: Option<String>,
}

pub(super) fn session_summary(
    token_usage: TokenUsage,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<SessionSummary> {
    let usage_line = (!token_usage.is_zero()).then(|| token_usage.to_string());
    let resume_hint = resumable_thread(thread_id, thread_name, rollout_path).and_then(|thread| {
        codex_utils_cli::resume_hint(thread.thread_name.as_deref(), Some(thread.thread_id))
    });

    if usage_line.is_none() && resume_hint.is_none() {
        return None;
    }

    Some(SessionSummary {
        usage_line,
        resume_hint,
    })
}
