use pretty_assertions::assert_eq;

use super::*;

#[test]
fn parser_rejects_leading_option_like_guidance() {
    for args in ["-x", "--ask -x", "--deferx", "--ask --unknown"] {
        let option = args.split_whitespace().last().expect("option token");
        assert_eq!(
            parse_handoff_args(args),
            Err(HandoffParseError::UnknownOption(option.to_string()))
        );
    }
}

#[test]
fn parser_preserves_lone_dash_and_option_like_guidance_after_separator() {
    assert_eq!(
        parse_handoff_args("-").expect("lone dash is guidance"),
        ParsedHandoffCommand {
            disposition: HandoffDisposition::Proceed,
            guidance: "-".to_string(),
            guidance_start: 0,
        }
    );
    assert_eq!(
        parse_handoff_args(" --ask -- -x ").expect("separator permits literal guidance"),
        ParsedHandoffCommand {
            disposition: HandoffDisposition::Ask,
            guidance: "-x".to_string(),
            guidance_start: 10,
        }
    );
}

#[test]
fn parser_accepts_deferred_handoff_and_rejects_conflicting_dispositions() {
    assert_eq!(
        parse_handoff_args("--defer finish after my next prompt").expect("deferred handoff"),
        ParsedHandoffCommand {
            disposition: HandoffDisposition::Defer,
            guidance: "finish after my next prompt".to_string(),
            guidance_start: 8,
        }
    );
    for args in ["--ask --defer", "--defer --ask"] {
        assert_eq!(
            parse_handoff_args(args),
            Err(HandoffParseError::ConflictingOptions)
        );
    }
    assert_eq!(
        parse_handoff_args("--defer -- --ask").expect("literal guidance"),
        ParsedHandoffCommand {
            disposition: HandoffDisposition::Defer,
            guidance: "--ask".to_string(),
            guidance_start: 11,
        }
    );
}

#[test]
fn parser_retains_guidance_offset_for_attached_command_content() {
    assert_eq!(
        parse_handoff_args("  --ask  keep the screenshot").expect("valid ask guidance"),
        ParsedHandoffCommand {
            disposition: HandoffDisposition::Ask,
            guidance: "keep the screenshot".to_string(),
            guidance_start: 9,
        }
    );
}

#[test]
fn handoff_fragment_obeys_existing_model_context_item_ceiling() {
    let framing_bytes = HandoffPlan {
        text: String::new(),
    }
    .execution_prompt()
    .len();
    let text = "a".repeat(MAX_CONTEXT_ITEM_TOKENS * 4 - framing_bytes);
    let plan = HandoffPlan::new(text.clone()).expect("fragment at ceiling is accepted");
    assert_eq!(
        approx_token_count(&plan.execution_prompt()),
        MAX_CONTEXT_ITEM_TOKENS
    );
    assert_eq!(
        HandoffPlan::new(format!("{text}a")),
        Err(HandoffPlanValidationError::ContextItemTooLarge {
            estimated_tokens: MAX_CONTEXT_ITEM_TOKENS + 1,
        })
    );
    assert_eq!(
        HandoffPlan::new(" \n ".to_string()),
        Err(HandoffPlanValidationError::Empty)
    );
}

#[test]
fn deferred_handoff_bounds_the_combined_plan_and_followup() {
    let plan = HandoffPlan::new("Preserve the current work.".to_string()).expect("valid plan");
    let framing_bytes = plan.execution_prompt_with_followup("").unwrap().len();
    let followup = "a".repeat(MAX_CONTEXT_ITEM_TOKENS * 4 - framing_bytes);
    assert_eq!(
        approx_token_count(&plan.execution_prompt_with_followup(&followup).unwrap()),
        MAX_CONTEXT_ITEM_TOKENS
    );
    assert_eq!(
        plan.execution_prompt_with_followup(&format!("{followup}a")),
        Err(HandoffPlanValidationError::ContextItemTooLarge {
            estimated_tokens: MAX_CONTEXT_ITEM_TOKENS + 1,
        })
    );
}
