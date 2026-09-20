use std::sync::Arc;

use codex_protocol::items::ExitedReviewModeItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::ReviewOutputEvent;
use tokio::sync::Mutex;
use tokio::sync::Notify;

use crate::context::PendingReviewReport;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

pub(super) struct ReviewExitCoordinator {
    state: Mutex<ReviewExitState>,
    finished: Notify,
}

struct ReviewExitState {
    item_id: String,
    output: Option<ReviewOutputEvent>,
    phase: ReviewExitPhase,
    in_progress: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReviewExitPhase {
    NotStarted,
    Started,
    Enqueued,
    Displayed,
    Persisted,
    Complete,
}

impl ReviewExitCoordinator {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ReviewExitState {
                item_id: uuid::Uuid::now_v7().to_string(),
                output: None,
                phase: ReviewExitPhase::NotStarted,
                in_progress: false,
            }),
            finished: Notify::new(),
        }
    }

    pub(super) async fn exit_once(
        self: &Arc<Self>,
        session: Arc<Session>,
        output: Option<ReviewOutputEvent>,
        ctx: Arc<TurnContext>,
    ) {
        let mut output = output;
        loop {
            let finished = self.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            let should_run = {
                let mut state = self.state.lock().await;
                if state.phase == ReviewExitPhase::Complete {
                    return;
                }
                if !state.in_progress {
                    if let Some(output) = output.take() {
                        state.output = Some(output);
                    }
                    state.in_progress = true;
                    true
                } else {
                    false
                }
            };
            if !should_run {
                finished.await;
                continue;
            }

            let coordinator = Arc::clone(self);
            let session = Arc::clone(&session);
            let ctx = Arc::clone(&ctx);
            let handle = tokio::spawn(async move {
                coordinator.finish(session, ctx).await;
            });
            if let Err(error) = handle.await {
                tracing::warn!(%error, "review exit finalization failed");
                let mut state = self.state.lock().await;
                state.in_progress = false;
                drop(state);
                self.finished.notify_waiters();
                continue;
            }
            return;
        }
    }

    pub(super) async fn remember_review_output(&self, output: &ReviewOutputEvent) {
        let mut state = self.state.lock().await;
        if state.phase != ReviewExitPhase::Complete && !state.in_progress {
            state.output = Some(output.clone());
        }
    }

    async fn finish(&self, session: Arc<Session>, ctx: Arc<TurnContext>) {
        loop {
            let (phase, item_id, output) = {
                let state = self.state.lock().await;
                (state.phase, state.item_id.clone(), state.output.clone())
            };
            let item = TurnItem::ExitedReviewMode(ExitedReviewModeItem {
                id: item_id.clone(),
                review_output: output.clone(),
            });
            let next_phase = match phase {
                ReviewExitPhase::NotStarted => {
                    session.emit_turn_item_started(ctx.as_ref(), &item).await;
                    ReviewExitPhase::Started
                }
                ReviewExitPhase::Started => {
                    enqueue_review_exit(session.as_ref(), &item_id, output.clone()).await;
                    ReviewExitPhase::Enqueued
                }
                ReviewExitPhase::Enqueued => {
                    session.emit_turn_item_completed(ctx.as_ref(), item).await;
                    ReviewExitPhase::Displayed
                }
                ReviewExitPhase::Displayed => {
                    persist_review_exit(session.as_ref(), &item_id, output).await;
                    ReviewExitPhase::Persisted
                }
                ReviewExitPhase::Persisted => ReviewExitPhase::Complete,
                ReviewExitPhase::Complete => ReviewExitPhase::Complete,
            };
            let mut state = self.state.lock().await;
            state.phase = next_phase;
            if next_phase == ReviewExitPhase::Complete {
                state.in_progress = false;
                drop(state);
                self.finished.notify_waiters();
                return;
            }
        }
    }
}

async fn persist_review_exit(
    session: &Session,
    item_id: &str,
    review_output: Option<ReviewOutputEvent>,
) {
    if let Some(output) = review_output {
        let report = PendingReviewReport::new(item_id.to_string(), output);
        if let Err(error) = session
            .try_persist_rollout_items(&[codex_protocol::protocol::RolloutItem::ResponseItem(
                crate::context::ReviewHandoff::pending_report_marker(&report),
            )])
            .await
        {
            tracing::warn!(%error, "failed to persist review report handoff state");
        }
    }
    session.ensure_rollout_materialized().await;
}

async fn enqueue_review_exit(
    session: &Session,
    item_id: &str,
    review_output: Option<ReviewOutputEvent>,
) {
    if let Some(output) = review_output {
        session
            .enqueue_review_report(PendingReviewReport::new(item_id.to_string(), output))
            .await;
    }
}
