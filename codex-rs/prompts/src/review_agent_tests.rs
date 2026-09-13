use super::*;

#[test]
fn every_stage_uses_shared_concise_rules() {
    for prompt in [
        REVIEW_PROMPT.to_string(),
        REVIEW_DOUBLE_CHECK_PROMPT.to_string(),
        review_fix_prompt(ReviewAction::Fix),
        review_fix_prompt(ReviewAction::FixAndCommit),
    ] {
        assert!(prompt.starts_with(SHARED_REVIEW_AGENT_PROMPT));
        assert!(prompt.contains("Return strict JSON only."));
    }
}

#[test]
fn discovery_omits_pre_existing_classification() {
    assert!(!REVIEW_PROMPT.contains("preExisting"));
    assert!(!REVIEW_PROMPT.contains("introduced by the selected change"));
    assert!(REVIEW_PROMPT.contains("Do not run a separate verification pass."));
}

#[test]
fn fix_commit_instructions_are_unambiguous() {
    let fix = review_fix_prompt(ReviewAction::Fix);
    assert!(fix.contains("Do not create a commit."));
    assert!(!fix.contains("Never amend"));

    let commit = review_fix_prompt(ReviewAction::FixAndCommit);
    assert!(commit.contains("Do not create, amend, or push a commit."));
    assert!(commit.contains("coordinator creates the focused commit"));
}

#[test]
fn repair_prompt_preserves_bounded_input_verbatim() {
    assert_eq!(
        review_repair_prompt("{schema}", "invalid"),
        "Your previous response did not match the required JSON schema.\n\nRepair its structure without adding new findings, removing substantive\ninformation, changing classifications, or expanding explanations. Preserve\nthe original technical meaning. Apply the shared concise-language rules.\n\nReturn only valid JSON matching this schema:\n{schema}\n\nPrevious response:\ninvalid"
    );
}
