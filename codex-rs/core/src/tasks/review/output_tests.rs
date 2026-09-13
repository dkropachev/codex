use super::*;

fn verified(pre_existing: ReviewPreExisting) -> VerifiedFinding {
    VerifiedFinding {
        candidate_index: 0,
        title: "Handle failure".to_string(),
        body: "The failure is ignored.".to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: StageCodeLocation {
            absolute_file_path: "/repo/src/lib.rs".to_string(),
            line_range: ReviewLineRange { start: 7, end: 7 },
        },
        pre_existing,
        pre_existing_fix_rationale: None,
    }
}

fn assessment() -> StageAssessment {
    StageAssessment {
        verdict: "patch is incorrect".to_string(),
        explanation: "One candidate remains.".to_string(),
        confidence_score: 0.8,
    }
}

#[test]
fn pull_request_classification_controls_sections() {
    let output = VerificationOutput {
        findings: vec![
            verified(ReviewPreExisting::False),
            verified(ReviewPreExisting::True),
            verified(ReviewPreExisting::Undetermined),
        ],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::PullRequest {
            url: "https://example.test/pull/1".to_string(),
        },
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(output.findings.len(), 1);
    assert_eq!(output.out_of_scope_findings.len(), 1);
    assert_eq!(output.unverified_findings.len(), 1);
}

#[test]
fn pre_existing_only_does_not_make_patch_incorrect() {
    let output = VerificationOutput {
        findings: vec![verified(ReviewPreExisting::True)],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::BaseBranch {
            branch: "main".to_string(),
        },
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(output.overall_correctness, "patch is correct");
    assert_eq!(
        output.overall_explanation,
        "No verified non-pre-existing findings remain."
    );
}

#[test]
fn omitted_candidates_make_an_empty_verification_uncertain() {
    let omitted = verified(ReviewPreExisting::Undetermined).into_review_finding();
    let output = VerificationOutput {
        findings: Vec::new(),
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::UncommittedChanges,
        Vec::new(),
        Vec::new(),
        vec![omitted],
    );

    assert_eq!(output.overall_correctness, "uncertain");
    assert_eq!(output.unverified_findings.len(), 1);
}

#[test]
fn candidate_bounding_reports_every_omitted_candidate() {
    let candidate = StageFinding {
        title: "Handle failure".to_string(),
        body: "x".repeat(1_024),
        confidence_score: 0.9,
        priority: 1,
        code_location: StageCodeLocation {
            absolute_file_path: "/repo/src/lib.rs".to_string(),
            line_range: ReviewLineRange { start: 7, end: 7 },
        },
    };
    let discovery = DiscoveryOutput {
        candidates: vec![candidate; 64],
        assessment: assessment(),
        review_context: Vec::new(),
        external_references: Vec::new(),
    };

    let bounded = discovery.bounded_candidates();

    assert!(!crate::context::bounded_candidates(&bounded.json).was_truncated());
    let included = serde_json::from_str::<Vec<serde_json::Value>>(&bounded.json)
        .expect("bounded candidates should remain valid JSON");
    assert!(
        included
            .iter()
            .all(|candidate| candidate["candidateIndex"].is_number())
    );
    assert!(!bounded.omitted.is_empty());
    assert_eq!(included.len() + bounded.omitted.len(), 64);
    assert_eq!(bounded.included_indices.len(), included.len());
}

#[test]
fn custom_review_preserves_an_explicit_baseline_classification() {
    let output = VerificationOutput {
        findings: vec![verified(ReviewPreExisting::True)],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::Custom {
            instructions: "Compare against merge base abc123.".to_string(),
        },
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::True);
    assert_eq!(output.overall_correctness, "patch is correct");
}

#[test]
fn verification_cannot_add_or_duplicate_candidate_locations() {
    let candidate = StageCodeLocation {
        absolute_file_path: "/repo/src/lib.rs".to_string(),
        line_range: ReviewLineRange { start: 7, end: 7 },
    };
    let mut duplicate = verified(ReviewPreExisting::False);
    duplicate.code_location = candidate.clone();
    let mut invented = verified(ReviewPreExisting::False);
    invented.code_location.line_range = ReviewLineRange { start: 99, end: 99 };
    let mut output = VerificationOutput {
        findings: vec![duplicate.clone(), duplicate, invented],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        assessment: assessment(),
    };

    output.retain_candidates(&[0]);

    assert_eq!(output.findings.len(), 1);
}
