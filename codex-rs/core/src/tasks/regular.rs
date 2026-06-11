use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::model_router::config_account_pool_default;
use crate::session::TurnInput;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelRerouteEvent;
use codex_protocol::protocol::ModelRerouteReason;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::WarningEvent;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskContext;

#[derive(Default)]
pub(crate) struct RegularTask;

impl RegularTask {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let sess = session.clone_session();
        let turn_extension_data = session.turn_extension_data();
        let previous_model = ctx.model_info.slug.clone();
        let previous_provider_id = ctx.config.model_provider_id.clone();
        let previous_account_pool = config_account_pool_default(&ctx.config);
        let previous_service_tier = ctx.config.service_tier.clone();
        let previous_reasoning_effort = ctx.config.model_reasoning_effort.clone();
        let ctx = if ctx
            .config
            .model_router
            .as_ref()
            .is_some_and(|router| router.enabled)
        {
            sess.route_regular_turn_context_for_model_router(ctx, &input)
                .await
        } else {
            ctx
        };
        let current_account_pool = config_account_pool_default(&ctx.config);
        let model_router_route_changed = ctx.model_info.slug != previous_model
            || ctx.config.model_provider_id != previous_provider_id
            || current_account_pool != previous_account_pool
            || ctx.config.service_tier != previous_service_tier
            || ctx.config.model_reasoning_effort != previous_reasoning_effort;
        let run_turn_span = trace_span!("run_turn");
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let event = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: ctx.sub_id.clone(),
            trace_id: ctx.trace_id.clone(),
            started_at: ctx.turn_timing_state.started_at_unix_secs().await,
            model_context_window: ctx.model_context_window(),
            collaboration_mode_kind: ctx.collaboration_mode.mode,
        });
        sess.send_event(ctx.as_ref(), event).await;
        if model_router_route_changed {
            let current_model = ctx.model_info.slug.clone();
            if current_model != previous_model {
                sess.send_event(
                    ctx.as_ref(),
                    EventMsg::ModelReroute(ModelRerouteEvent {
                        from_model: previous_model.clone(),
                        to_model: current_model,
                        reason: ModelRerouteReason::ModelRouterPolicy,
                    }),
                )
                .await;
            } else {
                let display_optional = |value: Option<&str>| value.unwrap_or("default").to_string();
                let previous_reasoning =
                    previous_reasoning_effort.as_ref().map(ToString::to_string);
                let current_reasoning = ctx
                    .config
                    .model_reasoning_effort
                    .as_ref()
                    .map(ToString::to_string);
                let mut changes = Vec::new();
                if ctx.config.model_provider_id != previous_provider_id {
                    changes.push(format!(
                        "provider {} -> {}",
                        previous_provider_id, ctx.config.model_provider_id
                    ));
                }
                if current_account_pool != previous_account_pool {
                    changes.push(format!(
                        "account label {} -> {}",
                        display_optional(previous_account_pool.as_deref()),
                        display_optional(current_account_pool.as_deref())
                    ));
                }
                if ctx.config.service_tier != previous_service_tier {
                    changes.push(format!(
                        "service tier {} -> {}",
                        display_optional(previous_service_tier.as_deref()),
                        display_optional(ctx.config.service_tier.as_deref())
                    ));
                }
                if current_reasoning != previous_reasoning {
                    changes.push(format!(
                        "reasoning effort {} -> {}",
                        display_optional(previous_reasoning.as_deref()),
                        display_optional(current_reasoning.as_deref())
                    ));
                }
                sess.send_event(
                    ctx.as_ref(),
                    EventMsg::Warning(WarningEvent {
                        message: format!("Model router updated this turn: {}.", changes.join(", ")),
                    }),
                )
                .await;
            }
        }
        sess.set_server_reasoning_included(/*included*/ false).await;
        let prewarmed_client_session = match sess
            .consume_startup_prewarm_for_regular_turn(&cancellation_token)
            .await
        {
            SessionStartupPrewarmResolution::Cancelled => return None,
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(mut prewarmed_client_session) => {
                if model_router_route_changed {
                    prewarmed_client_session.reset_websocket_session();
                    None
                } else {
                    Some(*prewarmed_client_session)
                }
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        loop {
            let last_agent_message = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                Arc::clone(&turn_extension_data),
                next_input,
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await;
            if !sess.input_queue.has_pending_input(&sess.active_turn).await {
                return last_agent_message;
            }
            next_input = Vec::new();
        }
    }
}
