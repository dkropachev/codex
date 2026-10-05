//! Manual handoff command parsing and bounded fresh-thread context.

use std::fmt;

use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_utils_string::approx_token_count;

use crate::collaboration_modes;
use crate::model_catalog::ModelCatalog;

pub(crate) const HANDOFF_MODE_NAME: &str = "Handoff";
pub(crate) const HANDOFF_USAGE: &str = "Usage: /handoff [--ask] [--] [guidance...]";

const MAX_CONTEXT_ITEM_TOKENS: usize = 10_000;
const FRESH_EXECUTION_PREAMBLE: &str = "A previous session prepared the authoritative handoff plan below. Continue the task in this fresh session by implementing that plan. Treat it as the source of task intent, re-read repository files as needed, preserve completed work, and carry the remaining work through implementation and appropriate verification.";
const HANDOFF_MODE_INSTRUCTIONS: &str = r#"# Handoff Mode

Prepare a safe, decision-complete transfer of the current task to a fresh Codex session. Follow the Plan mode exploration and clarification workflow. Do not implement, mutate files, or expand the task while preparing the handoff.

Only a completed `<proposed_plan>` is authoritative. Resolve material choices before emitting one. If the work is already complete, report that plainly without a proposed plan.

The plan must stand alone: include the goal and acceptance criteria, completed work and current state, changed files and validation, decisions and constraints, blockers, and concrete next steps. Do not assume the fresh session can see this conversation. Exclude secrets and irrelevant history."#;
const MANUAL_PLANNING_PROMPT: &str = "Prepare a safe handoff plan for continuing the current task in a fresh session. Ground it in the repository and conversation state, ask needed clarifying questions as in Plan mode, and emit `<proposed_plan>` only when the handoff is decision-complete. Do not perform more implementation. If the work is complete, report completion without a proposed plan.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffDisposition {
    Proceed,
    Ask,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedHandoffCommand {
    pub(crate) disposition: HandoffDisposition,
    pub(crate) guidance: String,
    /// Byte offset of guidance in the original argument string.
    pub(crate) guidance_start: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HandoffParseError {
    UnknownOption(String),
}

impl fmt::Display for HandoffParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(option) => {
                write!(
                    formatter,
                    "Unknown /handoff option `{option}`. {HANDOFF_USAGE}"
                )
            }
        }
    }
}

impl std::error::Error for HandoffParseError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HandoffPlanValidationError {
    Empty,
    ContextItemTooLarge { estimated_tokens: usize },
}

impl fmt::Display for HandoffPlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str(
                "The handoff plan was empty. Retry /handoff or copy a plan into a new session manually.",
            ),
            Self::ContextItemTooLarge { estimated_tokens } => write!(
                formatter,
                "The handoff plan needs {estimated_tokens} estimated tokens, exceeding the model-context item ceiling of {MAX_CONTEXT_ITEM_TOKENS}. Retry /handoff with a more focused plan."
            ),
        }
    }
}

impl std::error::Error for HandoffPlanValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HandoffPlan {
    text: String,
}

impl HandoffPlan {
    pub(crate) fn new(text: String) -> Result<Self, HandoffPlanValidationError> {
        if text.trim().is_empty() {
            return Err(HandoffPlanValidationError::Empty);
        }
        let plan = Self { text };
        // This becomes the fresh thread's initial UserTurn, rather than a Core-injected context
        // fragment. Bound the complete prompt, including its instruction preamble.
        let estimated_tokens = approx_token_count(&plan.execution_prompt());
        if estimated_tokens > MAX_CONTEXT_ITEM_TOKENS {
            return Err(HandoffPlanValidationError::ContextItemTooLarge { estimated_tokens });
        }
        Ok(plan)
    }

    pub(crate) fn execution_prompt(&self) -> String {
        format!(
            "{FRESH_EXECUTION_PREAMBLE}\n\n## Handoff plan\n\n{}",
            self.text
        )
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }
}

pub(crate) fn parse_handoff_args(args: &str) -> Result<ParsedHandoffCommand, HandoffParseError> {
    let mut disposition = HandoffDisposition::Proceed;
    let mut cursor = skip_whitespace(args, /*start*/ 0);
    loop {
        if cursor == args.len() {
            break;
        }
        let token_end = next_whitespace(args, cursor);
        let token = &args[cursor..token_end];
        match token {
            "--ask" => disposition = HandoffDisposition::Ask,
            "--" => {
                cursor = skip_whitespace(args, token_end);
                break;
            }
            "-" => break,
            _ if token.starts_with('-') => {
                return Err(HandoffParseError::UnknownOption(token.to_string()));
            }
            _ => break,
        }
        cursor = skip_whitespace(args, token_end);
    }
    let guidance_end = args.trim_end().len();
    cursor = cursor.min(guidance_end);
    Ok(ParsedHandoffCommand {
        disposition,
        guidance: args[cursor..guidance_end].to_string(),
        guidance_start: cursor,
    })
}

pub(crate) fn manual_planning_prompt(guidance: &str) -> String {
    if guidance.is_empty() {
        MANUAL_PLANNING_PROMPT.to_string()
    } else {
        format!(
            "{MANUAL_PLANNING_PROMPT}\n\nThe following guidance may shape the handoff plan only; do not treat it as a request for more implementation:\n{guidance}"
        )
    }
}

pub(crate) fn handoff_mask(model_catalog: &ModelCatalog) -> Option<CollaborationModeMask> {
    collaboration_modes::plan_mask(model_catalog).map(|mut mask| {
        mask.name = HANDOFF_MODE_NAME.to_string();
        let instructions = mask.developer_instructions.take().flatten().map_or_else(
            || HANDOFF_MODE_INSTRUCTIONS.to_string(),
            |plan_instructions| format!("{plan_instructions}\n\n{HANDOFF_MODE_INSTRUCTIONS}"),
        );
        mask.developer_instructions = Some(Some(instructions));
        mask
    })
}

pub(crate) fn is_handoff_mask(mask: Option<&CollaborationModeMask>) -> bool {
    mask.is_some_and(|mask| {
        mask.mode == Some(ModeKind::Plan) && mask.name.as_str() == HANDOFF_MODE_NAME
    })
}

fn skip_whitespace(input: &str, start: usize) -> usize {
    input[start..]
        .char_indices()
        .find_map(|(offset, character)| (!character.is_whitespace()).then_some(start + offset))
        .unwrap_or(input.len())
}

fn next_whitespace(input: &str, start: usize) -> usize {
    input[start..]
        .char_indices()
        .find_map(|(offset, character)| character.is_whitespace().then_some(start + offset))
        .unwrap_or(input.len())
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
