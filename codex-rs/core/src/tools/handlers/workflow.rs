use std::collections::BTreeMap;

use crate::function_tool::FunctionCallError;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowInputSource;
use codex_workflows::discover_workflow_tools;
use codex_workflows::execute_workflow_command;
use serde_json::to_string_pretty;
use tokio::task::spawn_blocking;

pub struct WorkflowHandler;

impl ToolHandler for WorkflowHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            tool_name,
            payload,
            ..
        } = invocation;
        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "workflow tools expect JSON function arguments".to_string(),
            ));
        };

        let workflow = resolve_workflow_tool(turn.as_ref(), &tool_name.name).await?;
        let output = run_workflow(
            turn.as_ref(),
            session.conversation_id.to_string(),
            workflow.workflow.id.clone(),
            arguments,
        )
        .await?;
        let output = to_string_pretty(&output).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to serialize workflow `{}` output: {err}",
                workflow.workflow.id
            ))
        })?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}

async fn resolve_workflow_tool(
    turn: &TurnContext,
    tool_name: &str,
) -> Result<codex_workflows::WorkflowPublishedTool, FunctionCallError> {
    let discovered = discover_workflow_tools(
        turn.config.codex_home.as_path(),
        turn.cwd.as_path(),
        &turn.config.workflows,
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to discover workflow tools for `{tool_name}`: {err:#}"
        ))
    })?;
    discovered
        .into_iter()
        .find(|workflow_tool| workflow_tool.tool_name() == tool_name)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("unknown workflow tool `{tool_name}`"))
        })
}

async fn run_workflow(
    turn: &TurnContext,
    session_id: String,
    workflow_id: String,
    arguments: String,
) -> Result<codex_workflows::WorkflowCommandOutput, FunctionCallError> {
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    let config = turn.config.workflows.clone();
    let codex_self_exe = turn.config.codex_self_exe.clone();

    spawn_blocking(move || {
        execute_workflow_command(
            WorkflowCommandContext {
                codex_home: codex_home.as_path(),
                cwd: cwd.as_path(),
                config: &config,
                codex_self_exe,
                stage_session_id: Some(session_id),
            },
            WorkflowCommand::Run {
                id: workflow_id,
                input: Some(WorkflowInputSource::Inline(arguments)),
                input_fields: BTreeMap::new(),
            },
        )
    })
    .await
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to run workflow tool: {err}"))
    })?
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("workflow tool execution failed: {err:#}"))
    })
}
