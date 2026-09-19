use super::*;

#[test]
fn every_stage_uses_shared_concise_rules() {
    for prompt in [
        REVIEW_PROMPT.to_string(),
        REVIEW_DOUBLE_CHECK_PROMPT.to_string(),
        REVIEW_FIX_SCOPE_PROMPT.to_string(),
        review_fix_prompt(ReviewAction::Fix),
        review_fix_prompt(ReviewAction::FixAndCommit),
    ] {
        assert!(prompt.starts_with(SHARED_REVIEW_AGENT_PROMPT));
        assert!(prompt.contains("Return only schema-valid JSON."));
    }
}

#[test]
fn discovery_omits_pre_existing_classification() {
    assert!(!REVIEW_PROMPT.contains("preExisting"));
    assert!(!REVIEW_PROMPT.contains("introduced by the selected change"));
    assert!(REVIEW_PROMPT.contains("Do not run a separate"));
    assert!(REVIEW_PROMPT.contains("DoubleCheck"));
}

#[test]
fn fix_commit_instructions_are_unambiguous() {
    let fix = review_fix_prompt(ReviewAction::Fix);
    assert!(fix.contains("Do not create a commit."));
    assert!(!fix.contains("Never amend"));

    let commit = review_fix_prompt(ReviewAction::FixAndCommit);
    assert!(commit.contains("Do not create, amend, or push a commit."));
    assert!(commit.contains("coordinator creates one focused commit"));
}

#[test]
fn repair_prompt_preserves_bounded_input_verbatim() {
    assert_eq!(
        review_repair_prompt(),
        "Repair <review_repair_input> to match the response schema exactly."
    );
    assert!(!review_repair_prompt().contains("{schema}"));
    assert!(REVIEW_REPAIR_PROMPT.starts_with(SHARED_REVIEW_AGENT_PROMPT));
}

#[test]
fn stage_prompts_remain_compact() {
    assert!(
        SHARED_REVIEW_AGENT_PROMPT.len() <= 500,
        "shared prompt is {} bytes",
        SHARED_REVIEW_AGENT_PROMPT.len()
    );
    assert!(
        REVIEW_PROMPT.len() <= 1_600,
        "review prompt is {} bytes",
        REVIEW_PROMPT.len()
    );
    assert!(
        REVIEW_DOUBLE_CHECK_PROMPT.len() <= 1_800,
        "double-check prompt is {} bytes",
        REVIEW_DOUBLE_CHECK_PROMPT.len()
    );
    assert!(
        REVIEW_FIX_SCOPE_PROMPT.len() <= 1_450,
        "fix scope prompt is {} bytes",
        REVIEW_FIX_SCOPE_PROMPT.len()
    );
    assert!(
        review_fix_prompt(ReviewAction::Fix).len() <= 1_600,
        "fix prompt is {} bytes",
        review_fix_prompt(ReviewAction::Fix).len()
    );
    assert!(
        review_fix_prompt(ReviewAction::FixAndCommit).len() <= 1_700,
        "fix+commit prompt is {} bytes",
        review_fix_prompt(ReviewAction::FixAndCommit).len()
    );
    assert!(REVIEW_REPAIR_PROMPT.len() + review_repair_prompt().len() <= 950);
}
