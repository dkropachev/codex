use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;
use crate::protocol::ReviewCodeLocation;
use crate::protocol::ReviewExternalReference;
use crate::protocol::ReviewLineRange;
use crate::protocol::ReviewReference;
use crate::protocol::ReviewResolution;
use crate::protocol::ReviewTestResult;

fn finding(title: &str, pre_existing: ReviewPreExisting) -> ReviewFinding {
    ReviewFinding {
        title: title.to_string(),
        body: "The error is ignored and the request can hang.".to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: ReviewCodeLocation {
            absolute_file_path: PathBuf::from("src/lib.rs"),
            line_range: ReviewLineRange { start: 10, end: 14 },
        },
        pre_existing,
        pre_existing_fix_rationale: None,
    }
}

#[test]
fn report_sections_follow_the_stable_order() {
    let output = ReviewOutputEvent {
        findings: vec![finding("Handle the failed send", ReviewPreExisting::False)],
        overall_correctness: "patch is incorrect".to_string(),
        overall_explanation: "One bug remains.".to_string(),
        overall_confidence_score: 0.9,
        out_of_scope_findings: vec![finding("Close the stale handle", ReviewPreExisting::True)],
        unverified_findings: vec![finding(
            "Check the platform fallback",
            ReviewPreExisting::Undetermined,
        )],
        references: vec![ReviewReference {
            reference: "src/large.rs:1-900".to_string(),
            explanation: "The requested range exceeds 400 lines.".to_string(),
        }],
        external_references: vec![ReviewExternalReference {
            reference: "upstream API".to_string(),
            explanation: "Its contract affects this call.".to_string(),
        }],
        resolution: Some(ReviewResolution {
            status: ReviewResolutionStatus::Complete,
            fixed_count: 1,
            rejected_count: 0,
            unresolved_count: 0,
            summary: vec!["Handled the failed send.".to_string()],
            tests: vec![ReviewTestResult {
                command: "just test -p codex-core".to_string(),
                status: ReviewTestStatus::Passed,
            }],
            commit_sha: Some("abc123".to_string()),
        }),
    };

    let report = render_review_output_text(&output);
    let headings = [
        "Assessment before fixes",
        "Findings",
        "Out-of-scope findings",
        "Unverified",
        "References",
        "External references",
        "Resolution",
    ];
    let positions =
        headings.map(|heading| report.find(heading).expect("heading should be present"));
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(report.contains("[P1] Handle the failed send — src/lib.rs:10-14"));
    assert!(report.contains("Pre-existing: no"));
}

#[test]
fn empty_optional_sections_are_omitted() {
    let output = ReviewOutputEvent {
        overall_correctness: "patch is correct".to_string(),
        overall_explanation: "No issues found.".to_string(),
        overall_confidence_score: 1.0,
        ..Default::default()
    };

    assert_eq!(
        render_review_output_text(&output),
        "Assessment\n\npatch is correct (confidence 1.00)\nNo issues found."
    );
}

#[test]
fn structured_priority_overrides_a_legacy_title_prefix() {
    let mut finding = finding("[P3] Handle the failed send", ReviewPreExisting::False);
    finding.priority = 0;

    assert!(
        format_review_findings_block(&[finding], /*selection*/ None)
            .starts_with("[P0] Handle the failed send")
    );
}

#[test]
fn pre_existing_rationale_is_rendered_once() {
    let mut finding = finding("Close the stale handle", ReviewPreExisting::True);
    finding.pre_existing_fix_rationale = Some("The adjacent change can fix it safely.".to_string());

    let report = format_review_findings_block(&[finding], /*selection*/ None);

    assert!(report.contains("Pre-existing: yes — The adjacent change can fix it safely."));
    assert_eq!(
        report
            .matches("The adjacent change can fix it safely.")
            .count(),
        1
    );
}
