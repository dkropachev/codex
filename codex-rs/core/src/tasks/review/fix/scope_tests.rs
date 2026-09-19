use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewLineRange;
use pretty_assertions::assert_eq;

use super::*;

fn finding() -> ReviewFinding {
    ReviewFinding {
        title: "Handle the failed send".to_string(),
        body: "The failed send is ignored.".to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: ReviewCodeLocation {
            absolute_file_path: PathBuf::from("src/lib.rs"),
            line_range: ReviewLineRange { start: 4, end: 4 },
        },
        pre_existing: ReviewPreExisting::Undetermined,
        pre_existing_fix_rationale: None,
    }
}

fn classification(
    finding_index: usize,
    pre_existing: ReviewPreExisting,
    validity: FixScopeValidity,
) -> FixScopeClassification {
    FixScopeClassification {
        finding_index,
        pre_existing,
        pre_existing_fix_rationale: None,
        validity,
    }
}

#[test]
fn mixed_scope_passes_only_valid_introduced_findings_to_mutation() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(), finding(), finding()],
        ..Default::default()
    };
    let plan = apply_scope_output(
        &ReviewTarget::UncommittedChanges,
        &mut output,
        &[0, 1, 2],
        FixScopeOutput {
            has_comparison_baseline: true,
            classifications: vec![
                classification(
                    /*finding_index*/ 0,
                    ReviewPreExisting::False,
                    FixScopeValidity::Valid,
                ),
                classification(
                    /*finding_index*/ 1,
                    ReviewPreExisting::True,
                    FixScopeValidity::Valid,
                ),
                classification(
                    /*finding_index*/ 2,
                    ReviewPreExisting::False,
                    FixScopeValidity::Rejected,
                ),
            ],
        },
        /*omitted_count*/ 0,
    )
    .expect("scope plan");

    assert_eq!(plan.mutable_indices, vec![0]);
    assert_eq!(plan.report_only_count, 1);
    assert_eq!(plan.rejected_count, 1);
}

#[test]
fn baseline_free_scope_requires_undetermined_classification() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding()],
        ..Default::default()
    };
    let error = apply_scope_output(
        &ReviewTarget::WholeRepository,
        &mut output,
        &[0],
        FixScopeOutput {
            has_comparison_baseline: false,
            classifications: vec![classification(
                /*finding_index*/ 0,
                ReviewPreExisting::False,
                FixScopeValidity::Valid,
            )],
        },
        /*omitted_count*/ 0,
    )
    .expect_err("invalid baseline-free classification");

    assert!(error.to_string().contains("baseline-free"));
}

#[test]
fn duplicate_scope_indices_are_rejected() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(), finding()],
        ..Default::default()
    };
    let error = apply_scope_output(
        &ReviewTarget::UncommittedChanges,
        &mut output,
        &[0, 1],
        FixScopeOutput {
            has_comparison_baseline: true,
            classifications: vec![
                classification(
                    /*finding_index*/ 0,
                    ReviewPreExisting::False,
                    FixScopeValidity::Valid,
                ),
                classification(
                    /*finding_index*/ 0,
                    ReviewPreExisting::False,
                    FixScopeValidity::Valid,
                ),
            ],
        },
        /*omitted_count*/ 0,
    )
    .expect_err("duplicate classification");

    assert!(error.to_string().contains("exactly once"));
}
