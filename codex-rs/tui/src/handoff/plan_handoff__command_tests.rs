use super::*;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

use crate::bottom_pane::slash_commands::BuiltinCommandFlags;
use crate::bottom_pane::slash_commands::builtins_for_input;
use crate::slash_command::SlashCommand;

#[test]
fn slash_command_registry_matches_plan_availability() {
    assert_eq!(
        SlashCommand::Handoff.supports_inline_args(),
        SlashCommand::Plan.supports_inline_args()
    );
    assert_eq!(
        SlashCommand::Handoff.available_during_task(),
        SlashCommand::Plan.available_during_task()
    );
    assert_eq!(
        SlashCommand::Handoff.available_in_side_conversation(),
        SlashCommand::Plan.available_in_side_conversation()
    );

    let disabled = builtins_for_input(BuiltinCommandFlags::default());
    let disabled_commands = disabled
        .iter()
        .map(|(_, command)| *command)
        .collect::<Vec<_>>();
    assert_eq!(
        disabled_commands.contains(&SlashCommand::Handoff),
        disabled_commands.contains(&SlashCommand::Plan)
    );
    let enabled = builtins_for_input(BuiltinCommandFlags {
        collaboration_modes_enabled: true,
        ..BuiltinCommandFlags::default()
    });
    let enabled_commands = enabled
        .iter()
        .map(|(_, command)| *command)
        .collect::<Vec<_>>();
    assert_eq!(
        enabled_commands.contains(&SlashCommand::Handoff),
        enabled_commands.contains(&SlashCommand::Plan)
    );
}

#[test]
fn parses_disposition_and_guidance() {
    assert_eq!(
        parse_handoff_args("keep the parser focused"),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Proceed,
            guidance: "keep the parser focused".to_string(),
            guidance_start: 0,
        })
    );
    assert_eq!(
        parse_handoff_args("  --ask  keep the parser focused  "),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Ask,
            guidance: "keep the parser focused".to_string(),
            guidance_start: 9,
        })
    );
    assert_eq!(
        parse_handoff_args("--defer"),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Defer,
            guidance: String::new(),
            guidance_start: 7,
        })
    );
}

#[test]
fn option_terminator_makes_option_like_text_guidance() {
    assert_eq!(
        parse_handoff_args("--ask -- --defer carefully"),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Ask,
            guidance: "--defer carefully".to_string(),
            guidance_start: 9,
        })
    );
    assert_eq!(
        parse_handoff_args("first --defer remains guidance"),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Proceed,
            guidance: "first --defer remains guidance".to_string(),
            guidance_start: 0,
        })
    );
    assert_eq!(
        parse_handoff_args("--   "),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Proceed,
            guidance: String::new(),
            guidance_start: 2,
        })
    );
}

#[test]
fn rejects_conflicting_and_unknown_leading_options() {
    assert_eq!(
        parse_handoff_args("--ask --defer guidance"),
        Err(HandoffParseError::ConflictingDispositionOptions)
    );
    assert_eq!(
        parse_handoff_args("--later guidance"),
        Err(HandoffParseError::UnknownOption("--later".to_string()))
    );
    assert!(
        HandoffParseError::UnknownOption("--later".to_string())
            .to_string()
            .contains(HANDOFF_USAGE)
    );
    assert_eq!(
        parse_handoff_args("- keep this as guidance"),
        Ok(ParsedHandoffCommand {
            disposition: HandoffDisposition::Proceed,
            guidance: "- keep this as guidance".to_string(),
            guidance_start: 0,
        })
    );
}

#[test]
fn guidance_offset_is_a_byte_offset_and_prompt_ends_with_guidance() {
    let args = "\u{2003}--defer\u{2003}résumé 中文  ";
    let parsed = parse_handoff_args(args).expect("arguments should parse");

    assert_eq!(
        &args[parsed.guidance_start..args.trim_end().len()],
        parsed.guidance
    );
    let prompt = manual_planning_prompt(&parsed.guidance);
    assert!(prompt.ends_with(&parsed.guidance));
    assert_eq!(
        &prompt[prompt.len() - parsed.guidance.len()..],
        parsed.guidance
    );
}

#[test]
fn validates_exact_utf8_byte_boundary() {
    let exactly_at_limit = "é".repeat(MAX_HANDOFF_PLAN_BYTES / "é".len());
    assert_eq!(exactly_at_limit.len(), MAX_HANDOFF_PLAN_BYTES);
    assert_eq!(validate_handoff_plan(&exactly_at_limit), Ok(()));

    let over_limit = format!("{exactly_at_limit}é");
    assert_eq!(
        validate_handoff_plan(&over_limit),
        Err(HandoffPlanValidationError::TooLarge {
            bytes: MAX_HANDOFF_PLAN_BYTES + "é".len(),
        })
    );
    assert_eq!(
        validate_handoff_plan(" \n\t"),
        Err(HandoffPlanValidationError::Empty)
    );
}

#[test]
fn pending_plan_preserves_text_and_formats_fresh_execution() {
    let plan = "# Continue\n\n1. Implement it.".to_string();
    let pending = PendingHandoffPlan::new(plan.clone()).expect("plan should be valid");

    assert_eq!(pending.plan(), plan);
    assert!(pending.execution_prompt().ends_with(&plan));
    assert!(
        pending
            .execution_prompt_with_instruction("Also keep the API private.")
            .ends_with("Also keep the API private.")
    );
    assert_eq!(pending.into_plan(), plan);
}

#[test]
fn handoff_mask_preserves_plan_settings_and_appends_requirements() {
    let plan_mask = CollaborationModeMask {
        name: "Plan".to_string(),
        mode: Some(ModeKind::Plan),
        model: Some("planner".to_string()),
        reasoning_effort: Some(Some(ReasoningEffort::High)),
        developer_instructions: Some(Some("original plan instructions".to_string())),
    };

    let handoff = handoff_mask_from_plan_mask(plan_mask);

    assert_eq!(handoff.name, HANDOFF_MODE_NAME);
    assert_eq!(handoff.mode, Some(ModeKind::Plan));
    assert_eq!(handoff.model.as_deref(), Some("planner"));
    assert_eq!(handoff.reasoning_effort, Some(Some(ReasoningEffort::High)));
    let instructions = handoff
        .developer_instructions
        .expect("override should be present")
        .expect("instructions should be present");
    assert!(instructions.starts_with("original plan instructions"));
    for requirement in [
        "goal and acceptance criteria",
        "completed work and the current state",
        "changed files and validation results",
        "decisions already made and constraints",
        "blockers or unresolved risks",
        "ordered, concrete next steps",
    ] {
        assert!(
            instructions.contains(requirement),
            "missing requirement: {requirement}"
        );
    }
}

#[test]
fn telemetry_dimensions_are_bounded_and_content_free() {
    let mut events = vec![
        HandoffTelemetryEvent::Trigger(HandoffTrigger::Manual),
        HandoffTelemetryEvent::Trigger(HandoffTrigger::Automatic),
    ];
    for trigger in [HandoffTrigger::Manual, HandoffTrigger::Automatic] {
        for disposition in [
            HandoffTelemetryDisposition::Proceed,
            HandoffTelemetryDisposition::Ask,
            HandoffTelemetryDisposition::Defer,
            HandoffTelemetryDisposition::Stay,
        ] {
            events.push(HandoffTelemetryEvent::Disposition {
                trigger,
                disposition,
            });
            events.push(HandoffTelemetryEvent::Completion {
                trigger,
                disposition: HandoffDisposition::Proceed,
            });
        }
        for reason in HandoffTelemetryReason::ALL {
            events.push(HandoffTelemetryEvent::Cancellation { trigger, reason });
            events.push(HandoffTelemetryEvent::Failure { trigger, reason });
        }
    }

    for event in events {
        let (name, tags) = event.dimensions();
        assert!(name.starts_with("codex.tui.handoff."));
        assert!(tags.len() <= 2);
        for (key, value) in tags {
            assert!(matches!(key, "source" | "disposition" | "reason"));
            assert!(!value.contains("user-authored-secret"));
            assert!(value.len() <= 24);
        }
    }
}
