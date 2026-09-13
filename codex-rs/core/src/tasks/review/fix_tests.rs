use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewResolution;
use codex_protocol::protocol::ReviewTestResult;
use pretty_assertions::assert_eq;

use super::*;

fn finding(pre_existing: ReviewPreExisting) -> ReviewFinding {
    ReviewFinding {
        title: "Handle the failed send".to_string(),
        body: "The failed send is ignored.".to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: ReviewCodeLocation {
            absolute_file_path: PathBuf::from("src/lib.rs"),
            line_range: ReviewLineRange { start: 4, end: 4 },
        },
        pre_existing,
        pre_existing_fix_rationale: None,
    }
}

#[tokio::test]
async fn interruption_waits_for_commit_finalization() {
    let state = Arc::new(super::super::ReviewFixFinalization::default());
    let guard = state.begin();
    let finished = Arc::new(AtomicBool::new(false));
    let waiter = {
        let state = Arc::clone(&state);
        let finished = Arc::clone(&finished);
        tokio::spawn(async move {
            state.wait().await;
            finished.store(true, Ordering::Release);
        })
    };

    tokio::task::yield_now().await;
    assert!(!finished.load(Ordering::Acquire));
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("finalization waiter timed out")
        .expect("finalization waiter failed");
    assert!(finished.load(Ordering::Acquire));
}

fn applied(dispositions: &[(usize, FixDisposition)]) -> super::super::output::AppliedFixOutput {
    super::super::output::AppliedFixOutput {
        dispositions: dispositions.iter().copied().collect::<HashMap<_, _>>(),
        invalid_structure: false,
    }
}

#[test]
fn single_pass_lets_fix_classify_provisional_baseline_findings() {
    let findings = vec![finding(ReviewPreExisting::Undetermined)];
    assert_eq!(
        eligible_finding_indices(
            &ReviewTarget::BaseBranch {
                branch: "main".to_string(),
            },
            ReviewVerification::SinglePass,
            &findings,
        ),
        vec![0]
    );
    assert!(
        eligible_finding_indices(
            &ReviewTarget::BaseBranch {
                branch: "main".to_string(),
            },
            ReviewVerification::DoubleCheck,
            &findings,
        )
        .is_empty()
    );
}

#[test]
fn failed_verification_prevents_fix_commit() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(ReviewPreExisting::False)],
        resolution: Some(ReviewResolution {
            status: ReviewResolutionStatus::Complete,
            fixed_count: 1,
            rejected_count: 0,
            unresolved_count: 0,
            summary: vec!["Changed the send path.".to_string()],
            tests: vec![ReviewTestResult {
                command: "just test -p example".to_string(),
                status: ReviewTestStatus::Failed,
            }],
            commit_sha: Some("abc123".to_string()),
        }),
        ..Default::default()
    };

    normalize_resolution(
        ReviewAction::FixAndCommit,
        &ReviewTarget::WholeRepository,
        &[0],
        /*omitted_count*/ 0,
        &applied(&[(0, FixDisposition::Fixed)]),
        &super::super::stage::ReviewStageEvidence::default(),
        &mut output,
    );

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.status, ReviewResolutionStatus::Partial);
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.commit_sha, None);
}

#[test]
fn pull_request_post_fix_classification_moves_report_only_findings() {
    let mut output = ReviewOutputEvent {
        findings: vec![
            finding(ReviewPreExisting::False),
            finding(ReviewPreExisting::True),
            finding(ReviewPreExisting::Undetermined),
        ],
        ..Default::default()
    };

    normalize_post_fix_sections(
        &ReviewTarget::PullRequest {
            url: "https://example.test/pull/1".to_string(),
        },
        &mut output,
    );

    assert_eq!(output.findings.len(), 1);
    assert_eq!(output.out_of_scope_findings.len(), 1);
    assert_eq!(output.unverified_findings.len(), 1);
}

#[test]
fn custom_post_fix_classification_preserves_an_explicit_baseline() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(ReviewPreExisting::True)],
        ..Default::default()
    };
    let target = ReviewTarget::Custom {
        instructions: "Compare against merge base abc123.".to_string(),
    };

    normalize_post_fix_sections(&target, &mut output);
    normalize_review_assessment(&target, &mut output);

    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::True);
    assert_eq!(output.overall_correctness, "patch is correct");
}

#[test]
fn report_only_reclassification_cannot_be_counted_as_fixed() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(ReviewPreExisting::True)],
        resolution: Some(ReviewResolution {
            status: ReviewResolutionStatus::Complete,
            fixed_count: 1,
            rejected_count: 0,
            unresolved_count: 0,
            summary: vec!["Changed code.".to_string()],
            tests: vec![ReviewTestResult {
                command: "just test -p example".to_string(),
                status: ReviewTestStatus::Passed,
            }],
            commit_sha: Some("abc123".to_string()),
        }),
        ..Default::default()
    };

    normalize_resolution(
        ReviewAction::FixAndCommit,
        &ReviewTarget::WholeRepository,
        &[0],
        /*omitted_count*/ 0,
        &applied(&[(0, FixDisposition::Fixed)]),
        &super::super::stage::ReviewStageEvidence::default(),
        &mut output,
    );

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.status, ReviewResolutionStatus::Partial);
    assert_eq!(resolution.commit_sha, None);
}

#[test]
fn unrelated_rejection_cannot_hide_a_report_only_fixed_count() {
    let mut output = ReviewOutputEvent {
        findings: vec![
            finding(ReviewPreExisting::True),
            finding(ReviewPreExisting::False),
        ],
        resolution: Some(ReviewResolution {
            status: ReviewResolutionStatus::Complete,
            fixed_count: 1,
            rejected_count: 1,
            unresolved_count: 0,
            summary: Vec::new(),
            tests: vec![ReviewTestResult {
                command: "just test -p example".to_string(),
                status: ReviewTestStatus::Passed,
            }],
            commit_sha: None,
        }),
        ..Default::default()
    };

    normalize_resolution(
        ReviewAction::FixAndCommit,
        &ReviewTarget::WholeRepository,
        &[0, 1],
        /*omitted_count*/ 0,
        &applied(&[(0, FixDisposition::Fixed), (1, FixDisposition::Rejected)]),
        &super::super::stage::ReviewStageEvidence::default(),
        &mut output,
    );

    let resolution = output.resolution.expect("resolution");
    assert_eq!(resolution.fixed_count, 0);
    assert_eq!(resolution.rejected_count, 1);
    assert_eq!(resolution.unresolved_count, 1);
    assert_eq!(resolution.status, ReviewResolutionStatus::Partial);
}

#[test]
fn resolution_status_is_derived_from_normalized_counts() {
    let mut output = ReviewOutputEvent {
        findings: vec![finding(ReviewPreExisting::False)],
        resolution: Some(ReviewResolution {
            status: ReviewResolutionStatus::Failed,
            fixed_count: 0,
            rejected_count: 1,
            unresolved_count: 0,
            summary: Vec::new(),
            tests: Vec::new(),
            commit_sha: None,
        }),
        ..Default::default()
    };

    normalize_resolution(
        ReviewAction::Fix,
        &ReviewTarget::WholeRepository,
        &[0],
        /*omitted_count*/ 0,
        &applied(&[(0, FixDisposition::Rejected)]),
        &super::super::stage::ReviewStageEvidence::default(),
        &mut output,
    );

    assert_eq!(
        output.resolution.expect("resolution").status,
        ReviewResolutionStatus::Complete
    );
}
