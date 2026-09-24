use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::SessionTask;
use super::SessionTaskResult;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;

mod runtime;

use runtime::run_workflow_for_tui;

const WORKFLOW_OUTPUT_MAX_BYTES: usize = 40 * 1024;
const WORKFLOW_ERROR_MAX_BYTES: usize = 4 * 1024;

#[derive(Clone)]
pub(crate) struct WorkflowCommandTask {
    workflow_dir: PathBuf,
    input: Value,
}

impl WorkflowCommandTask {
    pub(crate) fn new(workflow_dir: PathBuf, input: Value) -> Self {
        Self {
            workflow_dir,
            input,
        }
    }
}

impl SessionTask for WorkflowCommandTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.workflow_command"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        turn_context: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        session.services.session_telemetry.counter(
            "codex.task.workflow_command",
            /*inc*/ 1,
            &[],
        );

        let event = EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_context.sub_id.clone(),
            trace_id: turn_context.trace_id.clone(),
            started_at: turn_context.turn_timing_state.started_at_unix_secs().await,
            model_context_window: turn_context.model_context_window(),
            collaboration_mode_kind: turn_context.mode,
        });
        session.send_event(turn_context.as_ref(), event).await;

        let markdown = match run_workflow_for_tui(
            &self.workflow_dir,
            &self.input,
            Arc::clone(&session),
            Arc::clone(&turn_context),
            &cancellation_token,
        )
        .await
        {
            Ok(Some(markdown)) => markdown,
            Ok(None) => return Ok(None),
            Err(message) => {
                session
                    .send_event(
                        turn_context.as_ref(),
                        EventMsg::Error(ErrorEvent {
                            message,
                            codex_error_info: None,
                        }),
                    )
                    .await;
                return Ok(None);
            }
        };

        Ok(Some(
            record_workflow_output(session, turn_context, markdown).await,
        ))
    }
}

pub(crate) async fn record_workflow_output(
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    markdown: String,
) -> String {
    let markdown = truncate_workflow_output(markdown);
    session
        .record_response_item_and_emit_turn_item(
            turn_context.as_ref(),
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: markdown.clone(),
                }],
                phase: Some(MessagePhase::FinalAnswer),
                internal_chat_message_metadata_passthrough: None,
            },
        )
        .await;
    session.ensure_rollout_materialized().await;
    markdown
}

fn truncate_workflow_output(mut text: String) -> String {
    if text.len() <= WORKFLOW_OUTPUT_MAX_BYTES {
        return text;
    }

    let notice = format!("\n\n[Workflow output truncated to {WORKFLOW_OUTPUT_MAX_BYTES} bytes.]");
    let max_text_bytes = WORKFLOW_OUTPUT_MAX_BYTES.saturating_sub(notice.len());
    let boundary = previous_char_boundary(&text, max_text_bytes);
    text.truncate(boundary);
    text.push_str(&notice);
    text
}

fn truncate_error_output(text: &str) -> String {
    if text.len() <= WORKFLOW_ERROR_MAX_BYTES {
        return text.to_string();
    }

    let notice = format!("\n[workflow error output truncated to {WORKFLOW_ERROR_MAX_BYTES} bytes]");
    let max_text_bytes = WORKFLOW_ERROR_MAX_BYTES.saturating_sub(notice.len());
    let mut output = text.to_string();
    let boundary = previous_char_boundary(&output, max_text_bytes);
    output.truncate(boundary);
    output.push_str(&notice);
    output
}

fn previous_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index = index.saturating_sub(1);
    }
    index
}

#[cfg(test)]
#[path = "workflow_command_tests.rs"]
mod tests;
