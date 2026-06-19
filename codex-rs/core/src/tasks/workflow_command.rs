use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use codex_async_utils::CancelErr;
use codex_async_utils::OrCancelExt;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use serde_json::Value;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::SessionTask;
use super::SessionTaskContext;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;

const WORKFLOW_OUTPUT_MAX_BYTES: usize = 40 * 1024;
const WORKFLOW_ERROR_MAX_BYTES: usize = 4 * 1024;

const WORKFLOW_TUI_RUNNER: &str = r#"
const path = await import("node:path");
const { pathToFileURL } = await import("node:url");

const rawInput = process.argv[1] ?? "{}";
const input = JSON.parse(rawInput);
const workflowModule = await import(pathToFileURL(path.join(process.cwd(), "src/workflow.ts")).href);
const workflow = workflowModule.default ?? workflowModule;
if (!workflow || typeof workflow.run !== "function" || typeof workflow.format !== "function") {
  throw new Error("Workflow must export run() and format().");
}

const context = { progress: () => {} };
if (input && typeof input === "object" && typeof input.workingDirectory === "string") {
  context.workingDirectory = input.workingDirectory;
  context.cwd = input.workingDirectory;
  context.currentWorkingDirectory = input.workingDirectory;
  context.repoRoot = input.workingDirectory;
}

const result = await workflow.run(context, input);
const formatted = await workflow.format(result, { format: "tui.markdown.v1" });
if (!formatted || typeof formatted.markdown !== "string") {
  throw new Error("Workflow formatter did not return markdown for tui.markdown.v1.");
}
process.stdout.write(formatted.markdown);
if (!formatted.markdown.endsWith("\n")) {
  process.stdout.write("\n");
}
"#;

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
        session: Arc<SessionTaskContext>,
        turn_context: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        session.session.services.session_telemetry.counter(
            "codex.task.workflow_command",
            /*inc*/ 1,
            &[],
        );

        let markdown = match run_workflow_for_tui(
            &self.workflow_dir,
            &self.input,
            &cancellation_token,
        )
        .await
        {
            Ok(Some(markdown)) => markdown,
            Ok(None) => return None,
            Err(message) => {
                session
                    .clone_session()
                    .send_event(
                        turn_context.as_ref(),
                        EventMsg::Error(ErrorEvent {
                            message,
                            codex_error_info: None,
                        }),
                    )
                    .await;
                return None;
            }
        };

        Some(record_workflow_output(session.clone_session(), turn_context, markdown).await)
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
            },
        )
        .await;
    session.ensure_rollout_materialized().await;
    markdown
}

async fn run_workflow_for_tui(
    workflow_dir: &Path,
    input: &Value,
    cancellation_token: &CancellationToken,
) -> Result<Option<String>, String> {
    let input_json = serde_json::to_string(input)
        .map_err(|err| format!("failed to serialize workflow input: {err}"))?;
    let mut command = Command::new("bun");
    command
        .current_dir(workflow_dir)
        .arg("--eval")
        .arg(WORKFLOW_TUI_RUNNER)
        .arg("--")
        .arg(input_json)
        .kill_on_drop(true);

    let output = match command.output().or_cancel(cancellation_token).await {
        Ok(Ok(output)) => output,
        Ok(Err(err)) => {
            return Err(format!("failed to start workflow command: {err}"));
        }
        Err(CancelErr::Cancelled) => return Ok(None),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let details = if !stderr.trim().is_empty() {
            stderr.as_ref()
        } else {
            stdout.as_ref()
        };
        let details = truncate_error_output(details);
        return Err(format!(
            "workflow command failed with status {}: {}",
            output.status.code().map_or_else(
                || "terminated by signal".to_string(),
                |code| code.to_string()
            ),
            details.trim()
        ));
    }

    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|err| format!("workflow output was not valid UTF-8: {err}"))
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
