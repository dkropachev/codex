use std::sync::Arc;

use codex_protocol::turn_input::NotSubmittedReason;

use super::session::Session;
use crate::state::ActiveTurn;
use crate::state::TurnState;

/// Atomically reserves the active-turn slot, then yields to trigger-turn mail
/// that arrived concurrently with the reservation.
pub(super) async fn reserve(
    session: &Arc<Session>,
) -> Result<Arc<tokio::sync::Mutex<TurnState>>, NotSubmittedReason> {
    let turn_state = {
        let mut active_turn = session.active_turn.lock().await;
        if active_turn.is_some() {
            return Err(NotSubmittedReason::NotIdle);
        }
        let active_turn = active_turn.get_or_insert_with(ActiveTurn::default);
        Arc::clone(&active_turn.turn_state)
    };

    if session.input_queue.has_trigger_turn_mailbox_items().await {
        clear(session, &turn_state).await;
        session.maybe_start_turn_for_pending_work().await;
        return Err(NotSubmittedReason::PendingTriggerTurn);
    }
    Ok(turn_state)
}

pub(super) async fn clear(session: &Session, turn_state: &Arc<tokio::sync::Mutex<TurnState>>) {
    let mut active_turn_guard = session.active_turn.lock().await;
    if let Some(active_turn) = active_turn_guard.as_ref()
        && active_turn.task.is_none()
        && Arc::ptr_eq(&active_turn.turn_state, turn_state)
    {
        *active_turn_guard = None;
    }
}
