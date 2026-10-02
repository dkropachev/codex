use std::fmt;

use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;

use crate::collaboration_modes;
use crate::model_catalog::ModelCatalog;

mod telemetry;

pub(crate) use telemetry::HandoffTelemetryDisposition;
pub(crate) use telemetry::HandoffTelemetryEvent;
pub(crate) use telemetry::HandoffTelemetryReason;

pub(crate) const HANDOFF_MODE_NAME: &str = "Handoff";
pub(crate) const MAX_HANDOFF_PLAN_BYTES: usize = 8 * 1024;
pub(crate) const HANDOFF_USAGE: &str = "Usage: /handoff [--ask | --defer] [--] [guidance...]";

const HANDOFF_MODE_INSTRUCTIONS: &str = r#"# Handoff Mode

You are preparing a safe, decision-complete transfer of the current task to a fresh Codex session. Follow the Plan mode exploration and clarification workflow. Do not implement, mutate files, or expand the task while preparing the handoff.

Only a completed `<proposed_plan>` is an authoritative handoff. Do not emit one until remaining material choices have been resolved. If the requested work is already complete, report that plainly without emitting `<proposed_plan>`.

The handoff plan must be self-contained and no more than 8192 UTF-8 bytes. Include:
- the goal and acceptance criteria;
- completed work and the current state;
- changed files and validation results;
- decisions already made and constraints that must be preserved;
- blockers or unresolved risks; and
- ordered, concrete next steps.

Do not assume the fresh session can see this conversation. Include the details it needs to continue safely, but never include secrets or irrelevant conversation history."#;

const MANUAL_PLANNING_PROMPT: &str = r#"Prepare a safe handoff plan for continuing the current task in a fresh session. Ground the plan in the repository and conversation state, ask any necessary clarifying questions exactly as in Plan mode, and emit `<proposed_plan>` only when the handoff is decision-complete. Do not perform additional implementation.

The complete plan must be no more than 8192 UTF-8 bytes and must record the goal, acceptance criteria, completed work, current state, changed files, validation results, decisions, constraints, blockers, ordered next steps, and everything the fresh session needs without access to this conversation. If the requested work is already complete, report completion without emitting `<proposed_plan>`."#;

pub(crate) const AUTOMATIC_WRAP_UP_PROMPT: &str = r#"Prepare this task for an automatic session handoff. Finish the current atomic work safely, run the relevant targeted validation, stop expanding scope, and record any blockers. Do not start unrelated work. When the atomic work is settled, report its current state so a focused handoff plan can be prepared."#;

pub(crate) const AUTOMATIC_PLANNING_PROMPT: &str = r#"Prepare a focused handoff plan for continuing this task in a fresh session. Keep the complete plan within 8192 UTF-8 bytes. Use the goal, completed work, current state, changed files, validation results, decisions, constraints, blockers, ordered next steps, and acceptance criteria from this session. Explore or ask necessary clarifying questions exactly as in Plan mode, but do not perform additional implementation. Emit `<proposed_plan>` only when the handoff is decision-complete. If the requested work is already complete, report completion without emitting `<proposed_plan>`."#;

const FRESH_EXECUTION_PREAMBLE: &str = r#"A previous session prepared the authoritative handoff plan below. Continue the task in this fresh session by implementing that plan. Treat it as the source of task intent, re-read repository files as needed, preserve completed work, and carry the remaining work through implementation and appropriate verification."#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffDisposition {
    Proceed,
    Ask,
    Defer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffTrigger {
    Manual,
    Automatic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HandoffPhase {
    WrappingUp,
    AwaitingPlanning,
    Planning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActiveHandoff {
    pub(crate) trigger: HandoffTrigger,
    pub(crate) disposition: HandoffDisposition,
    pub(crate) phase: HandoffPhase,
}

impl ActiveHandoff {
    pub(crate) fn manual(disposition: HandoffDisposition) -> Self {
        Self {
            trigger: HandoffTrigger::Manual,
            disposition,
            phase: HandoffPhase::Planning,
        }
    }

    pub(crate) fn automatic() -> Self {
        Self {
            trigger: HandoffTrigger::Automatic,
            disposition: HandoffDisposition::Proceed,
            phase: HandoffPhase::WrappingUp,
        }
    }

    pub(crate) fn begin_planning(&mut self) {
        self.phase = HandoffPhase::Planning;
    }

    pub(crate) fn await_planning_gate(&mut self) {
        self.phase = HandoffPhase::AwaitingPlanning;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedHandoffCommand {
    pub(crate) disposition: HandoffDisposition,
    pub(crate) guidance: String,
    /// Byte offset of `guidance` in the original argument string.
    pub(crate) guidance_start: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HandoffParseError {
    ConflictingDispositionOptions,
    UnknownOption(String),
}

impl fmt::Display for HandoffParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingDispositionOptions => write!(
                formatter,
                "`--ask` and `--defer` cannot be used together. {HANDOFF_USAGE}"
            ),
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
    TooLarge { bytes: usize },
}

impl fmt::Display for HandoffPlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str(
                "The handoff plan was empty; retry /handoff or copy a plan into a new session manually.",
            ),
            Self::TooLarge { bytes } => write!(
                formatter,
                "The handoff plan is {bytes} bytes, exceeding the {MAX_HANDOFF_PLAN_BYTES}-byte limit; retry /handoff with a more focused plan or transfer it manually."
            ),
        }
    }
}

impl std::error::Error for HandoffPlanValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingHandoffPlan {
    plan: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingHandoffState {
    plan: PendingHandoffPlan,
    submitted_text: Option<String>,
    completion: Option<(HandoffTrigger, HandoffDisposition)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PassiveHandoffState {
    pub(crate) context_hint_shown: bool,
    pub(crate) automatic_cancelled_until_rearm: bool,
    pub(crate) next_generation: u64,
}

impl PendingHandoffState {
    pub(crate) fn new(plan: PendingHandoffPlan) -> Self {
        Self {
            plan,
            submitted_text: None,
            completion: None,
        }
    }

    pub(crate) fn proceeding(plan: PendingHandoffPlan, trigger: HandoffTrigger) -> Self {
        Self {
            plan,
            submitted_text: None,
            completion: Some((trigger, HandoffDisposition::Proceed)),
        }
    }

    pub(crate) fn plan(&self) -> &PendingHandoffPlan {
        &self.plan
    }

    pub(crate) fn submitted_text(&self) -> Option<&str> {
        self.submitted_text.as_deref()
    }

    pub(crate) fn mark_submitted(&mut self, text: String) {
        self.submitted_text = Some(text);
    }

    pub(crate) fn mark_submission_failed(&mut self) {
        self.submitted_text = None;
    }

    pub(crate) fn completion(&self) -> Option<(HandoffTrigger, HandoffDisposition)> {
        self.completion
    }
}

impl PendingHandoffPlan {
    pub(crate) fn new(plan: String) -> Result<Self, HandoffPlanValidationError> {
        validate_handoff_plan(&plan)?;
        Ok(Self { plan })
    }

    pub(crate) fn plan(&self) -> &str {
        &self.plan
    }

    pub(crate) fn into_plan(self) -> String {
        self.plan
    }

    pub(crate) fn execution_prompt(&self) -> String {
        format!(
            "{FRESH_EXECUTION_PREAMBLE}\n\n## Handoff plan\n\n{}",
            self.plan
        )
    }

    pub(crate) fn execution_prompt_with_instruction(&self, instruction: &str) -> String {
        format!(
            "{}\n\n## New instruction\n\n{instruction}",
            self.execution_prompt()
        )
    }
}

pub(crate) fn validate_handoff_plan(plan: &str) -> Result<(), HandoffPlanValidationError> {
    if plan.trim().is_empty() {
        return Err(HandoffPlanValidationError::Empty);
    }
    let bytes = plan.len();
    if bytes > MAX_HANDOFF_PLAN_BYTES {
        return Err(HandoffPlanValidationError::TooLarge { bytes });
    }
    Ok(())
}

pub(crate) fn parse_handoff_args(args: &str) -> Result<ParsedHandoffCommand, HandoffParseError> {
    let mut disposition = HandoffDisposition::Proceed;
    let mut cursor = skip_whitespace(args, /*start*/ 0);

    loop {
        if cursor == args.len() {
            return Ok(ParsedHandoffCommand {
                disposition,
                guidance: String::new(),
                guidance_start: cursor,
            });
        }

        let token_start = cursor;
        let token_end = next_whitespace(args, token_start);
        let token = &args[token_start..token_end];
        match token {
            "--ask" => {
                if disposition == HandoffDisposition::Defer {
                    return Err(HandoffParseError::ConflictingDispositionOptions);
                }
                disposition = HandoffDisposition::Ask;
            }
            "--defer" => {
                if disposition == HandoffDisposition::Ask {
                    return Err(HandoffParseError::ConflictingDispositionOptions);
                }
                disposition = HandoffDisposition::Defer;
            }
            "--" => {
                cursor = skip_whitespace(args, token_end);
                break;
            }
            _ if token.starts_with("--") => {
                return Err(HandoffParseError::UnknownOption(token.to_string()));
            }
            _ => break,
        }
        cursor = skip_whitespace(args, token_end);
    }

    let guidance_end = args.trim_end().len();
    cursor = cursor.min(guidance_end);
    let guidance = args[cursor..guidance_end].to_string();
    Ok(ParsedHandoffCommand {
        disposition,
        guidance,
        guidance_start: cursor,
    })
}

pub(crate) fn manual_planning_prompt(guidance: &str) -> String {
    if guidance.is_empty() {
        MANUAL_PLANNING_PROMPT.to_string()
    } else {
        format!(
            "{MANUAL_PLANNING_PROMPT}\n\nThe following guidance may shape the handoff plan only; do not treat it as a request to perform more implementation:\n{guidance}"
        )
    }
}

pub(crate) fn handoff_mask(model_catalog: &ModelCatalog) -> Option<CollaborationModeMask> {
    collaboration_modes::plan_mask(model_catalog).map(handoff_mask_from_plan_mask)
}

pub(crate) fn is_handoff_mask(mask: Option<&CollaborationModeMask>) -> bool {
    mask.is_some_and(|mask| {
        mask.mode == Some(ModeKind::Plan) && mask.name.as_str() == HANDOFF_MODE_NAME
    })
}

fn handoff_mask_from_plan_mask(mut mask: CollaborationModeMask) -> CollaborationModeMask {
    mask.name = HANDOFF_MODE_NAME.to_string();
    let instructions = mask.developer_instructions.take().flatten().map_or_else(
        || HANDOFF_MODE_INSTRUCTIONS.to_string(),
        |plan_instructions| format!("{plan_instructions}\n\n{HANDOFF_MODE_INSTRUCTIONS}"),
    );
    mask.developer_instructions = Some(Some(instructions));
    mask
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
#[path = "handoff/plan_handoff__command_tests.rs"]
mod tests;
