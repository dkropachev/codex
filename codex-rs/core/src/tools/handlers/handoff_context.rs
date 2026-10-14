use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_utils_output_truncation::approx_token_count;
use serde::Deserialize;
use std::collections::BTreeMap;

const TOOL_NAME: &str = "handoff_context";
const MAX_SUMMARY_TOKENS: usize = 10_000;

pub struct HandoffContextHandler;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    summary: String,
}

impl ToolExecutor<ToolInvocation> for HandoffContextHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: TOOL_NAME.to_string(),
            description: "Checkpoint a concise handoff summary and continue in this thread with refreshed context. Include the active task, decisions, unfinished work, and identifiers for live agents and processes.".to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([("summary".to_string(), JsonSchema::string(Some("Handoff summary".to_string())))]),
                Some(vec!["summary".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Function { arguments } = invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "handoff_context requires function arguments".to_string(),
                ));
            };
            let Args { summary } = parse_arguments(&arguments)?;
            if summary.trim().is_empty() || approx_token_count(&summary) > MAX_SUMMARY_TOKENS {
                return Err(FunctionCallError::RespondToModel(format!(
                    "handoff summary must contain 1 to {MAX_SUMMARY_TOKENS} estimated tokens"
                )));
            }
            invocation.session.request_handoff_summary(summary).await;
            Ok(boxed_tool_output(FunctionToolOutput::from_text(
                "Context handoff queued.".to_string(),
                Some(true),
            )))
        })
    }
}

impl CoreToolRuntime for HandoffContextHandler {}
