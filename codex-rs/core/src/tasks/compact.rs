use std::sync::Arc;

use super::SessionTask;
use super::SessionTaskResult;
use super::emit_compact_metric;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_analytics::CompactionReason;
use codex_analytics::CompactionTrigger;
use codex_features::Feature;
use codex_model_provider::RemoteCompactionSupport;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::turn_input::CompactionSource;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
pub(crate) struct CompactTask {
    source: CompactionSource,
}

impl CompactTask {
    pub(crate) fn new(source: CompactionSource) -> Self {
        Self { source }
    }
}

impl SessionTask for CompactTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    fn span_name(&self) -> &'static str {
        "session_task.compact"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        _cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let _profile_guard = ctx.turn_timing_state.begin_compaction();
        let (trigger, reason, manual) = match self.source {
            CompactionSource::Manual => (
                CompactionTrigger::Manual,
                CompactionReason::UserRequested,
                true,
            ),
            CompactionSource::Automatic => (
                CompactionTrigger::Auto,
                CompactionReason::ContextLimit,
                false,
            ),
        };
        if ctx.config.features.enabled(Feature::TokenBudget) {
            emit_compact_metric(&session.services.session_telemetry, "token_budget", manual);
            crate::compact_token_budget::run_standalone_compact_task(session, ctx, trigger).await?;
            return Ok(None);
        }

        let result = match ctx.provider.capabilities().remote_compaction {
            RemoteCompactionSupport::V2 => {
                emit_compact_metric(&session.services.session_telemetry, "remote_v2", manual);
                crate::compact_remote_v2::run_remote_compact_task(
                    session.clone(),
                    ctx,
                    trigger,
                    reason,
                )
                .await
            }
            RemoteCompactionSupport::Unsupported => {
                emit_compact_metric(&session.services.session_telemetry, "local", manual);
                let input = vec![UserInput::Text {
                    text: ctx
                        .config
                        .compact_prompt
                        .as_deref()
                        .unwrap_or(crate::compact::SUMMARIZATION_PROMPT)
                        .to_string(),
                    // Compaction prompt is synthesized; no UI element ranges to preserve.
                    text_elements: Vec::new(),
                }];
                crate::compact::run_compact_task(session.clone(), ctx, input, trigger, reason).await
            }
        };
        if let Err(err) = result
            && matches!(err.details(), CodexErrorDetails::TurnAborted)
        {
            return Err(err);
        }
        Ok(None)
    }
}
