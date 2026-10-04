//! Handles reply-bearing turn-input operations.
//!
//! This is the one place Core decides whether submitted input starts a turn,
//! steers an active turn, or is rejected. It replies after that decision; it
//! does not wait for user-prompt hooks, updating the in-memory model context,
//! rollout persistence, or sampling.
//!
//! Persistent thread settings apply on Started and Steered. Turn start
//! options only apply on Started.
//! Host shutdown admission is checked before reserving or starting a new turn.
//! Parent-delegated subagent input bypasses drain; automatic starts remain gated.
//! Realtime drain refusals are returned to the fanout for ordered session teardown.

use super::TurnInput;
use super::idle_turn;
use super::session::Session;
use crate::context::GuardianContextMode;
use crate::tasks::RegularTask;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::NonSteerableTurnKind;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::TurnInput as SubmittedTurnInput;
use codex_protocol::turn_input::TurnInputMode;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::turn_input::TurnStartOptions;
use codex_protocol::user_input::UserInput;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[cfg(test)]
#[path = "turn_input_tests.rs"]
mod tests;

#[path = "turn_input_settings.rs"]
mod turn_input_settings;

use turn_input_settings::PreparedTurnInputSettings;
use turn_input_settings::TurnStartKind;

pub(super) async fn handle(
    session: &Arc<Session>,
    request: TurnInputRequest,
    mode: TurnInputMode,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let _turn_start_guard = session.acquire_turn_start_lock().await;
    match mode {
        TurnInputMode::StartOrSteer => start_or_steer(session, request, submission_id).await,
        TurnInputMode::StartIfIdle => {
            let kind = match &request.input {
                SubmittedTurnInput::UserInput { content, .. } if !content.is_empty() => {
                    TurnStartKind::User
                }
                SubmittedTurnInput::UserInput { .. }
                | SubmittedTurnInput::ResponseItem(_)
                | SubmittedTurnInput::InterAgentCommunication(_) => TurnStartKind::Automatic,
            };
            start_if_idle(
                session,
                request,
                submission_id,
                kind,
                /*expected_previous_turn_id*/ None,
            )
            .await
        }
        TurnInputMode::ContinueIfIdle {
            expected_previous_turn_id,
        } => {
            if !matches!(&request.input, SubmittedTurnInput::ResponseItem(_)) {
                return Err(CodexErr::InvalidRequest(
                    "continuation requires internal response input".to_string(),
                ));
            }
            start_if_idle(
                session,
                request,
                submission_id,
                TurnStartKind::Recovery,
                Some(expected_previous_turn_id),
            )
            .await
        }
        TurnInputMode::StartUserIfIdle => {
            user_turn_has_explicit_input(&request.input)?;
            start_if_idle(
                session,
                request,
                submission_id,
                TurnStartKind::User,
                /*expected_previous_turn_id*/ None,
            )
            .await
        }
        TurnInputMode::Steer { expected_turn_id } => {
            steer(session, request, expected_turn_id, submission_id).await
        }
    }
}

pub(super) async fn handle_recovery(
    session: &Arc<Session>,
    thread_settings: ThreadSettingsOverrides,
    start_options: TurnStartOptions,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let _turn_start_guard = session.acquire_turn_start_lock().await;
    let request = TurnInputRequest::user_input(Vec::new())
        .with_thread_settings(thread_settings)
        .on_start(TurnStartOptions {
            turn_trigger: Some("retry".to_string()),
            ..start_options
        });
    start_if_idle(
        session,
        request,
        submission_id,
        TurnStartKind::Recovery,
        /*expected_previous_turn_id*/ None,
    )
    .await
}

async fn start_or_steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        mut input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        app_server_client_info,
        trace: _,
    } = request;
    let has_explicit_input = user_turn_has_explicit_input(&input)?;
    let settings = PreparedTurnInputSettings::prepare(session, thread_settings, start)
        .await?
        .with_app_server_client_info(app_server_client_info);
    match session
        .steer_input(
            &mut input,
            additional_context.clone(),
            /*expected_turn_id*/ None,
            settings.required_active_final_output_json_schema(),
            responsesapi_client_metadata.clone(),
        )
        .await
    {
        Ok(turn_id) => {
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(NotSubmittedReason::NoActiveTurn) => {
            // MAv1 sends explicit input to spawned agents as part of an existing
            // parent's work. Client RPCs are gated separately by the host.
            let is_delegated_input = settings.start_options.parent_turn_id.is_some()
                && matches!(
                    session
                        .state
                        .lock()
                        .await
                        .session_configuration
                        .session_source,
                    SessionSource::SubAgent(SubAgentSource::ThreadSpawn { .. })
                );
            let _admission = if is_delegated_input {
                None
            } else {
                let Some(admission) = session.services.extensions.admit_turn_start() else {
                    return Ok(TurnInputSubmission::NotSubmitted {
                        reason: NotSubmittedReason::ServerDraining,
                    });
                };
                Some(admission)
            };
            let turn_state = match idle_turn::reserve_for_user_turn(session).await {
                Ok(turn_state) => turn_state,
                Err(reason) => return Ok(TurnInputSubmission::NotSubmitted { reason }),
            };
            let turn_context = match settings
                .apply_started(session, submission_id.clone(), TurnStartKind::User)
                .await
            {
                Ok(Some(turn_context)) => turn_context,
                Ok(None) => {
                    idle_turn::clear(session, &turn_state).await;
                    unreachable!("explicit user input can enter Plan mode");
                }
                Err(error) => {
                    idle_turn::clear(session, &turn_state).await;
                    return Err(error);
                }
            };
            if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
                turn_context
                    .turn_metadata_state
                    .set_responsesapi_client_metadata(responsesapi_client_metadata);
            }
            session
                .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
                .await;
            if let SubmittedTurnInput::UserInput { content, .. } = &input {
                turn_context.session_telemetry.user_prompt(content);
            }
            let mut task_input = merge_additional_context_input(session, additional_context).await;
            if has_explicit_input {
                task_input.push(pending_turn_input(session, input, &turn_context.sub_id).await);
            }
            session.clear_connector_selection().await;
            session
                .start_task(turn_context, task_input, RegularTask::new())
                .await;
            Ok(TurnInputSubmission::Started {
                turn_id: submission_id,
            })
        }
        Err(reason) => Ok(TurnInputSubmission::NotSubmitted { reason }),
    }
}

fn user_turn_has_explicit_input(input: &SubmittedTurnInput) -> CodexResult<bool> {
    match input {
        SubmittedTurnInput::UserInput { content, .. } => Ok(!content.is_empty()),
        SubmittedTurnInput::ResponseItem(ResponseItem::FunctionCallOutput {
            call_id: None,
            ..
        }) => Ok(true),
        SubmittedTurnInput::ResponseItem(_) | SubmittedTurnInput::InterAgentCommunication(_) => {
            Err(CodexErr::InvalidRequest(
                "only user input or standalone function-call outputs can start or steer a turn"
                    .to_string(),
            ))
        }
    }
}

async fn start_if_idle(
    session: &Arc<Session>,
    request: TurnInputRequest,
    submission_id: String,
    kind: TurnStartKind,
    expected_previous_turn_id: Option<String>,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        app_server_client_info,
        trace: _,
    } = request;
    if session.input_queue.has_trigger_turn_mailbox_items().await {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        });
    }
    // Preserve current-Plan rejection before reservation and settings errors.
    // The commit-time decision also checks the proposed mode.
    if kind == TurnStartKind::Automatic
        && !kind.permits_mode(session.collaboration_mode().await.mode)
    {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::PlanMode,
        });
    }

    let _admission = session.services.extensions.admit_turn_start();
    if _admission.is_none() {
        return Ok(TurnInputSubmission::NotSubmitted {
            reason: NotSubmittedReason::ServerDraining,
        });
    }

    let reservation = match expected_previous_turn_id.as_deref() {
        Some(expected_previous_turn_id) => {
            idle_turn::reserve_after(session, expected_previous_turn_id).await
        }
        None => idle_turn::reserve(session).await,
    };
    let turn_state = match reservation {
        Ok(turn_state) => turn_state,
        Err(reason) => return Ok(TurnInputSubmission::NotSubmitted { reason }),
    };

    let settings =
        match PreparedTurnInputSettings::prepare(session, thread_settings, start).await {
            Ok(settings) => settings,
            Err(error) => {
                idle_turn::clear(session, &turn_state).await;
                return Err(error);
            }
        }
        .with_app_server_client_info(app_server_client_info);
    let turn_context = match settings
        .apply_started(session, submission_id.clone(), kind)
        .await
    {
        Ok(Some(turn_context)) => turn_context,
        Ok(None) => {
            idle_turn::clear(session, &turn_state).await;
            return Ok(TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::PlanMode,
            });
        }
        Err(error) => {
            idle_turn::clear(session, &turn_state).await;
            return Err(error);
        }
    };
    if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
        turn_context
            .turn_metadata_state
            .set_responsesapi_client_metadata(responsesapi_client_metadata);
    }
    session
        .maybe_emit_model_warnings_for_turn(turn_context.as_ref())
        .await;

    let mut task_input = merge_additional_context_input(session, additional_context).await;
    match kind {
        TurnStartKind::User => {
            session.clear_connector_selection().await;
            if let SubmittedTurnInput::UserInput { content, .. } = &input {
                turn_context.session_telemetry.user_prompt(content);
            }
            if user_turn_has_explicit_input(&input)? {
                task_input.push(pending_turn_input(session, input, &turn_context.sub_id).await);
            }
        }
        TurnStartKind::Automatic | TurnStartKind::Recovery => {
            // Empty automatic user input resumes sampling without a new message.
            if !matches!(&input, SubmittedTurnInput::UserInput { .. }) {
                session
                    .input_queue
                    .extend_pending_input_for_turn_state(
                        turn_state.as_ref(),
                        vec![pending_turn_input(session, input, &turn_context.sub_id).await],
                    )
                    .await;
            }
        }
    }
    session
        .start_task(turn_context, task_input, RegularTask::new())
        .await;
    Ok(TurnInputSubmission::Started {
        turn_id: submission_id,
    })
}

async fn steer(
    session: &Arc<Session>,
    request: TurnInputRequest,
    expected_turn_id: String,
    submission_id: String,
) -> CodexResult<TurnInputSubmission> {
    let TurnInputRequest {
        mut input,
        thread_settings,
        start,
        additional_context,
        responsesapi_client_metadata,
        app_server_client_info,
        trace: _,
    } = request;
    if !matches!(&input, SubmittedTurnInput::UserInput { .. }) {
        return Err(CodexErr::InvalidRequest(
            "only user input can steer a turn".to_string(),
        ));
    }
    let settings = PreparedTurnInputSettings::prepare(session, thread_settings, start)
        .await?
        .with_app_server_client_info(app_server_client_info);
    match session
        .steer_input(
            &mut input,
            additional_context,
            Some(expected_turn_id.as_str()),
            settings.required_active_final_output_json_schema(),
            responsesapi_client_metadata,
        )
        .await
    {
        Ok(turn_id) => {
            settings.apply_steered(session, submission_id).await?;
            Ok(TurnInputSubmission::Steered { turn_id })
        }
        Err(reason) => Ok(TurnInputSubmission::NotSubmitted { reason }),
    }
}

impl Session {
    /// Called under the active-turn lock before running any task or lifecycle callback.
    pub(crate) async fn record_started_turn(&self, turn_id: &str) {
        self.state.lock().await.last_started_turn_id = Some(turn_id.to_string());
    }

    pub(crate) async fn route_realtime_text_input(
        self: &Arc<Self>,
        text: String,
    ) -> Result<(), &'static str> {
        let submission_id = Uuid::now_v7().to_string();
        let submission = handle(
            self,
            TurnInputRequest::user_input(vec![UserInput::Text {
                text,
                text_elements: Vec::new(),
            }])
            .on_start(TurnStartOptions {
                turn_trigger: Some("realtime".to_string()),
                ..Default::default()
            }),
            TurnInputMode::StartOrSteer,
            submission_id.clone(),
        )
        .await;
        match submission {
            Ok(TurnInputSubmission::Started { .. } | TurnInputSubmission::Steered { .. }) => {}
            Ok(TurnInputSubmission::NotSubmitted {
                reason: NotSubmittedReason::ServerDraining,
            }) => {
                return Err("Server is draining; retry the turn after reconnecting");
            }
            Ok(TurnInputSubmission::NotSubmitted { reason }) => {
                self.send_event_raw(Event {
                    id: submission_id,
                    msg: EventMsg::Error(ErrorEvent {
                        misalignment: None,
                        message: format!("failed to submit turn input: {reason:?}"),
                        codex_error_info: Some(CodexErrorInfo::BadRequest),
                    }),
                })
                .await;
            }
            Err(error) => {
                self.send_event_raw(Event {
                    id: submission_id,
                    msg: EventMsg::Error(error.to_error_event(/*message_prefix*/ None)),
                })
                .await;
            }
        }
        Ok(())
    }

    /// Inject additional user input or a standalone tool output into the active turn.
    ///
    /// Returns the active turn id when accepted.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    async fn steer_input(
        &self,
        input: &mut SubmittedTurnInput,
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        expected_turn_id: Option<&str>,
        required_final_output_json_schema: Option<&Value>,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
    ) -> Result<String, NotSubmittedReason> {
        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(NotSubmittedReason::NoActiveTurn);
        };

        let Some(active_task) = active_turn.task.as_ref() else {
            return Err(NotSubmittedReason::NotIdle);
        };
        let active_turn_id = &active_task.turn_context.sub_id;

        if let Some(expected_turn_id) = expected_turn_id
            && expected_turn_id != active_turn_id
        {
            return Err(NotSubmittedReason::ExpectedTurnMismatch {
                expected: expected_turn_id.to_string(),
                actual: active_turn_id.clone(),
            });
        }

        match active_task.kind {
            crate::state::TaskKind::Regular => {}
            crate::state::TaskKind::Compact => {
                return Err(NotSubmittedReason::ActiveTurnNotSteerable {
                    turn_kind: NonSteerableTurnKind::Compact,
                });
            }
        }

        if matches!(input, SubmittedTurnInput::UserInput { content, .. } if content.is_empty()) {
            return Err(NotSubmittedReason::EmptyInput);
        }
        // Compare JSON values directly instead of serialized schema text.
        // Value equality ignores object key order while preserving array and
        // scalar distinctions; broader JSON Schema equivalence is out of scope.
        if let Some(required_schema) = required_final_output_json_schema
            && active_task.turn_context.final_output_json_schema.as_ref() != Some(required_schema)
        {
            return Err(NotSubmittedReason::ActiveTurnOutputSchemaMismatch);
        }
        let mut pending_input = merge_additional_context_input(self, additional_context).await;

        if let Some(responsesapi_client_metadata) = responsesapi_client_metadata {
            active_task
                .turn_context
                .turn_metadata_state
                .set_responsesapi_client_metadata(responsesapi_client_metadata);
        }

        let input = match input {
            SubmittedTurnInput::UserInput { content, client_id } => {
                active_task
                    .turn_context
                    .session_telemetry
                    .user_prompt(content);
                TurnInput::UserInput {
                    content: std::mem::take(content),
                    client_id: client_id.clone(),
                    acceptance_order: self.reserve_user_input_order().await,
                }
            }
            input => pending_turn_input(self, input.clone(), active_turn_id).await,
        };
        pending_input.push(input);
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                active_turn.turn_state.as_ref(),
                pending_input,
            )
            .await;
        Ok(active_turn_id.clone())
    }
}

async fn merge_additional_context_input(
    session: &Session,
    additional_context: BTreeMap<String, AdditionalContextEntry>,
) -> Vec<TurnInput> {
    let additional_context_input = {
        let mut state = session.state.lock().await;
        state.additional_context.merge(additional_context)
    };
    additional_context_input
        .into_iter()
        .map(|item| session.annotate_client_response_item(item))
        .map(TurnInput::ResponseItem)
        .collect()
}

async fn pending_turn_input(
    session: &Session,
    input: SubmittedTurnInput,
    turn_id: &str,
) -> TurnInput {
    match input {
        SubmittedTurnInput::UserInput { content, client_id } => TurnInput::UserInput {
            content,
            client_id,
            acceptance_order: session.reserve_user_input_order().await,
        },
        SubmittedTurnInput::ResponseItem(mut item)
            if matches!(
                &item,
                ResponseItem::FunctionCallOutput { call_id: None, .. }
            ) =>
        {
            Session::assign_missing_response_item_id(&mut item);
            let metadata = if session.guardian_context_mode == GuardianContextMode::ThreadOwned
                && let Some(messages) = session
                    .services
                    .agent_control
                    .capture_sender_user_messages(&item, session.thread_id, turn_id)
                    .await
            {
                Some(CodexHarnessMetadata {
                    user_input_order: session.reserve_user_input_order().await,
                    sender_user_messages: Some(Box::new(messages)),
                    ..Default::default()
                })
            } else {
                None
            };
            TurnInput::FunctionCallOutput(ResponseItemEnvelope { item, metadata })
        }
        SubmittedTurnInput::ResponseItem(item) => TurnInput::ResponseItem(item.into()),
        SubmittedTurnInput::InterAgentCommunication(communication) => {
            TurnInput::InterAgentCommunication(communication)
        }
    }
}
