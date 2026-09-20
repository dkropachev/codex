use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn legacy_review_request_defaults_chain_settings() {
    let request: ReviewRequest = serde_json::from_value(json!({
        "target": {"type": "custom", "instructions": "check this"},
        "user_facing_hint": null
    }))
    .expect("legacy request");

    assert_eq!(request.verification, ReviewVerification::SinglePass);
    assert_eq!(request.action, ReviewAction::Report);
}

#[test]
fn legacy_review_output_defaults_extended_fields() {
    let output: ReviewOutputEvent = serde_json::from_value(json!({
        "findings": [{
            "title": "Handle the error",
            "body": "The error is ignored.",
            "confidence_score": 0.9,
            "priority": 1,
            "code_location": {
                "absolute_file_path": "/repo/src/lib.rs",
                "line_range": {"start": 4, "end": 4}
            }
        }],
        "overall_correctness": "patch is incorrect",
        "overall_explanation": "One issue remains.",
        "overall_confidence_score": 0.9
    }))
    .expect("legacy output");

    assert_eq!(
        output.findings[0].pre_existing,
        ReviewPreExisting::Undetermined
    );
    assert!(output.out_of_scope_findings.is_empty());
    assert!(output.unverified_findings.is_empty());
    assert!(output.references.is_empty());
    assert!(output.external_references.is_empty());
    assert_eq!(output.resolution, None);
}

#[test]
fn whole_repository_target_uses_camel_case_wire_type() {
    assert_eq!(
        serde_json::to_value(ReviewTarget::WholeRepository).expect("serialize target"),
        json!({"type": "wholeRepository"})
    );
}
