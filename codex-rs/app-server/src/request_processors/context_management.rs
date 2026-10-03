use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadCompactStartSource;
use codex_core::CodexThread;
use codex_core::CompactionRequest;
use codex_core::CompactionSource;
use codex_core::NotSubmittedReason;
use codex_core::StartIfIdleSubmission;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::protocol::W3cTraceContext;

use super::internal_error;
use super::invalid_request;

pub(super) struct AcceptedTurnInput {
    pub(super) turn_id: String,
    pub(super) started: bool,
}

pub(super) async fn start_compaction(
    thread: &CodexThread,
    source: Option<ThreadCompactStartSource>,
    trace: Option<W3cTraceContext>,
) -> Result<String, JSONRPCErrorError> {
    let source = match source.unwrap_or_default() {
        ThreadCompactStartSource::Manual => CompactionSource::Manual,
        ThreadCompactStartSource::AutomaticContextManagement => CompactionSource::Automatic,
    };
    let submission = thread
        .compact_if_idle(CompactionRequest { source, trace })
        .await
        .map_err(|err| internal_error(format!("failed to start compaction: {err}")))?;
    match submission {
        StartIfIdleSubmission::Started { turn_id } => Ok(turn_id),
        StartIfIdleSubmission::NotSubmitted { reason } => {
            let reason_debug = format!("{reason:?}");
            match reason {
                NotSubmittedReason::ServerDraining => {
                    Err(crate::error_code::server_draining_error())
                }
                NotSubmittedReason::NotIdle | NotSubmittedReason::PendingTriggerTurn => Err(
                    invalid_request("thread already has an active or pending turn"),
                ),
                NotSubmittedReason::Superseded
                | NotSubmittedReason::PlanMode
                | NotSubmittedReason::NoActiveTurn
                | NotSubmittedReason::ExpectedTurnMismatch { .. }
                | NotSubmittedReason::ActiveTurnNotSteerable { .. }
                | NotSubmittedReason::ActiveTurnOutputSchemaMismatch
                | NotSubmittedReason::EmptyInput => Err(internal_error(format!(
                    "Core declined to start compaction: {reason_debug}"
                ))),
            }
        }
    }
}

pub(super) async fn submit_turn(
    thread: &CodexThread,
    request: TurnInputRequest,
    start_if_idle: bool,
) -> Result<AcceptedTurnInput, JSONRPCErrorError> {
    let submission = if start_if_idle {
        thread
            .start_user_turn_if_idle(request)
            .await
            .map(|submission| match submission {
                StartIfIdleSubmission::Started { turn_id } => {
                    TurnInputSubmission::Started { turn_id }
                }
                StartIfIdleSubmission::NotSubmitted { reason } => {
                    TurnInputSubmission::NotSubmitted { reason }
                }
            })
    } else {
        thread.start_or_steer_turn(request).await
    }
    .map_err(|err| internal_error(format!("failed to submit turn input: {err}")))?;

    match submission {
        TurnInputSubmission::Started { turn_id } => Ok(AcceptedTurnInput {
            turn_id,
            started: true,
        }),
        TurnInputSubmission::Steered { turn_id } => Ok(AcceptedTurnInput {
            turn_id,
            started: false,
        }),
        TurnInputSubmission::NotSubmitted { reason } => {
            let reason_debug = format!("{reason:?}");
            match reason {
                NotSubmittedReason::ServerDraining => {
                    Err(crate::error_code::server_draining_error())
                }
                NotSubmittedReason::NotIdle | NotSubmittedReason::PendingTriggerTurn
                    if start_if_idle =>
                {
                    Err(invalid_request(
                        "thread already has an active or pending turn",
                    ))
                }
                NotSubmittedReason::Superseded
                | NotSubmittedReason::NotIdle
                | NotSubmittedReason::PendingTriggerTurn
                | NotSubmittedReason::PlanMode
                | NotSubmittedReason::NoActiveTurn
                | NotSubmittedReason::ExpectedTurnMismatch { .. }
                | NotSubmittedReason::ActiveTurnNotSteerable { .. }
                | NotSubmittedReason::ActiveTurnOutputSchemaMismatch
                | NotSubmittedReason::EmptyInput => Err(internal_error(format!(
                    "failed to submit turn input: {reason_debug}"
                ))),
            }
        }
    }
}
