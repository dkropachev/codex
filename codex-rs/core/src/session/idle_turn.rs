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
    reserve_with_precondition(session, ReservationPrecondition::Any).await
}

/// Reserves the idle slot only if no turn has started after `expected_previous_turn_id`.
pub(super) async fn reserve_after(
    session: &Arc<Session>,
    expected_previous_turn_id: &str,
) -> Result<Arc<tokio::sync::Mutex<TurnState>>, NotSubmittedReason> {
    reserve_with_precondition(
        session,
        ReservationPrecondition::PreviousTurn(expected_previous_turn_id),
    )
    .await
}

enum ReservationPrecondition<'a> {
    Any,
    PreviousTurn(&'a str),
}

#[expect(
    clippy::await_holding_invalid_type,
    reason = "the previous turn check and idle reservation must be atomic"
)]
async fn reserve_with_precondition(
    session: &Arc<Session>,
    precondition: ReservationPrecondition<'_>,
) -> Result<Arc<tokio::sync::Mutex<TurnState>>, NotSubmittedReason> {
    let turn_state = {
        let mut active_turn = session.active_turn.lock().await;
        if active_turn.is_some() {
            return Err(NotSubmittedReason::NotIdle);
        }
        match precondition {
            ReservationPrecondition::Any => {}
            ReservationPrecondition::PreviousTurn(expected_previous_turn_id) => {
                if session.state.lock().await.last_started_turn_id.as_deref()
                    != Some(expected_previous_turn_id)
                {
                    return Err(NotSubmittedReason::Superseded);
                }
            }
        }
        let active_turn = active_turn.get_or_insert_with(ActiveTurn::default);
        Arc::clone(&active_turn.turn_state)
    };

    if session.input_queue.has_trigger_turn_mailbox_items().await {
        clear(session, &turn_state).await;
        let session = Arc::clone(session);
        drop(tokio::spawn(async move {
            session.maybe_start_turn_for_pending_work().await;
        }));
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
