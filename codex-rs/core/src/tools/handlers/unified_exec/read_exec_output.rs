use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::PostToolUsePayload;
use crate::tools::registry::PreToolUsePayload;
use crate::tools::registry::ToolExecutor;
use crate::unified_exec::resolve_max_tokens;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text;
use regex_lite::Regex;
use serde::Deserialize;
use std::collections::BTreeSet;

use super::super::shell_spec::create_read_exec_output_tool;

const DEFAULT_SEARCH_CONTEXT_LINES: usize = 2;

#[derive(Debug, Deserialize)]
struct ReadExecOutputArgs {
    chunk_id: String,
    #[serde(default)]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    line_start: Option<usize>,
    #[serde(default)]
    line_count: Option<usize>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    context_lines: Option<usize>,
}

pub struct ReadExecOutputHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for ReadExecOutputHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("read_exec_output")
    }

    fn spec(&self) -> ToolSpec {
        create_read_exec_output_tool()
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "read_exec_output handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: ReadExecOutputArgs = parse_arguments(&arguments)?;
        if args.pattern.is_some() && (args.line_start.is_some() || args.line_count.is_some()) {
            return Err(FunctionCallError::RespondToModel(
                "read_exec_output cannot combine `pattern` search with `line_start`/`line_count` slicing.".to_string(),
            ));
        }
        if matches!(args.line_start, Some(0)) {
            return Err(FunctionCallError::RespondToModel(
                "read_exec_output `line_start` must be at least 1.".to_string(),
            ));
        }

        let Some(raw_output) = session
            .services
            .unified_exec_manager
            .read_archived_output(args.chunk_id.as_str())
            .await
        else {
            return Err(FunctionCallError::RespondToModel(format!(
                "no archived exec output found for chunk_id `{}`",
                args.chunk_id
            )));
        };

        let raw_text = String::from_utf8_lossy(raw_output.as_slice()).to_string();
        let selected = if let Some(pattern) = args.pattern {
            search_output(
                raw_text.as_str(),
                pattern.as_str(),
                args.context_lines.unwrap_or(DEFAULT_SEARCH_CONTEXT_LINES),
            )?
        } else {
            slice_output(raw_text.as_str(), args.line_start, args.line_count)
        };
        let max_tokens =
            resolve_max_tokens(args.max_output_tokens).min(turn.truncation_policy.token_budget());
        let output = formatted_truncate_text(&selected, TruncationPolicy::Tokens(max_tokens));
        let text = format!("Chunk ID: {}\nOutput:\n{output}", args.chunk_id);

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            text,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for ReadExecOutputHandler {
    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. })
    }

    fn pre_tool_use_payload(&self, _invocation: &ToolInvocation) -> Option<PreToolUsePayload> {
        None
    }

    fn post_tool_use_payload(
        &self,
        _invocation: &ToolInvocation,
        _result: &dyn crate::tools::context::ToolOutput,
    ) -> Option<PostToolUsePayload> {
        None
    }
}

fn slice_output(output: &str, line_start: Option<usize>, line_count: Option<usize>) -> String {
    if line_start.is_none() && line_count.is_none() {
        return output.to_string();
    }

    let start = line_start.unwrap_or(1).saturating_sub(1);
    output
        .lines()
        .skip(start)
        .take(line_count.unwrap_or(usize::MAX))
        .collect::<Vec<_>>()
        .join("\n")
}

fn search_output(
    output: &str,
    pattern: &str,
    context_lines: usize,
) -> Result<String, FunctionCallError> {
    let regex = Regex::new(pattern).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid read_exec_output pattern: {err}"))
    })?;
    let lines: Vec<&str> = output.lines().collect();
    let mut keep = BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if regex.is_match(line) {
            let start = idx.saturating_sub(context_lines);
            let end = idx
                .saturating_add(context_lines)
                .min(lines.len().saturating_sub(1));
            for line_idx in start..=end {
                keep.insert(line_idx);
            }
        }
    }

    if keep.is_empty() {
        return Ok(String::new());
    }

    let mut rendered = Vec::new();
    let mut previous = None;
    for idx in keep {
        if previous.is_some_and(|previous_idx| idx.saturating_sub(previous_idx) > 1) {
            rendered.push("[... omitted lines ...]".to_string());
        }
        rendered.push(lines[idx].to_string());
        previous = Some(idx);
    }
    Ok(rendered.join("\n"))
}
