use std::sync::Arc;

use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::turn_input::CompactionSource;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::StartIfIdleSubmission;

use super::idle_turn;
use super::session::Session;
use crate::tasks::CompactTask;

pub(super) async fn run(session: &Arc<Session>, submission_id: String) {
    let _turn_start_guard = session.acquire_turn_start_lock().await;
    session.abort_all_tasks(TurnAbortReason::Replaced).await;
    let turn_context = session
        .new_turn_with_default_settings(submission_id, Default::default())
        .await;
    session
        .spawn_task(
            turn_context,
            Vec::new(),
            CompactTask::new(CompactionSource::Manual),
        )
        .await;
}

pub(super) async fn run_if_idle(
    session: &Arc<Session>,
    submission_id: String,
    source: CompactionSource,
) -> StartIfIdleSubmission {
    let _turn_start_guard = session.acquire_turn_start_lock().await;
    if session.input_queue.has_trigger_turn_mailbox_items().await {
        return StartIfIdleSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        };
    }
    let Some(_admission) = session.services.extensions.admit_turn_start() else {
        return StartIfIdleSubmission::NotSubmitted {
            reason: NotSubmittedReason::ServerDraining,
        };
    };
    if let Err(reason) = idle_turn::reserve(session).await {
        return StartIfIdleSubmission::NotSubmitted { reason };
    }

    session.clear_connector_selection().await;
    let turn_context = session
        .new_turn_with_default_settings(submission_id.clone(), Default::default())
        .await;
    session
        .start_task(turn_context, Vec::new(), CompactTask::new(source))
        .await;
    StartIfIdleSubmission::Started {
        turn_id: submission_id,
    }
}
