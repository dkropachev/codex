use super::*;

fn verified(pre_existing: ReviewPreExisting) -> VerifiedFinding {
    VerifiedFinding {
        candidate_index: 0,
        pre_existing,
        pre_existing_fix_rationale: None,
    }
}

fn candidate() -> StageFinding {
    StageFinding {
        title: "Handle failure".to_string(),
        body: "The failure is ignored.".to_string(),
        confidence_score: 0.9,
        priority: 1,
        code_location: StageCodeLocation {
            absolute_file_path: "/repo/src/lib.rs".to_string(),
            line_range: ReviewLineRange { start: 7, end: 7 },
        },
    }
}

fn assessment() -> StageAssessment {
    StageAssessment {
        verdict: "patch is incorrect".to_string(),
        explanation: "One candidate remains.".to_string(),
        confidence_score: 0.8,
    }
}

fn classified_finding() -> ReviewFinding {
    let mut finding = candidate().into_review_finding();
    finding.pre_existing = ReviewPreExisting::False;
    finding.pre_existing_fix_rationale = Some("Introduced by this change.".to_string());
    finding
}

fn classification_update(finding_index: usize) -> ClassificationUpdate {
    ClassificationUpdate {
        finding_index,
        pre_existing: ReviewPreExisting::False,
        pre_existing_fix_rationale: Some("Introduced by this change.".to_string()),
        disposition: FixDisposition::Fixed,
    }
}

fn fix_resolution() -> FixResolution {
    FixResolution {
        status: ReviewResolutionStatus::Complete,
        fixed_count: 1,
        rejected_count: 0,
        unresolved_count: 0,
        summary: vec!["Fixed the finding.".to_string()],
        tests: vec![FixTestResult {
            command: "just test".to_string(),
            status: ReviewTestStatus::Passed,
        }],
        commit_sha: None,
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
        rejected_candidate_indices: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::PullRequest {
            url: "https://example.test/pull/1".to_string(),
        },
        &[candidate()],
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
        rejected_candidate_indices: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::BaseBranch {
            branch: "main".to_string(),
        },
        &[candidate()],
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
    let omitted = candidate().into_review_finding();
    let output = VerificationOutput {
        findings: Vec::new(),
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        rejected_candidate_indices: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::UncommittedChanges,
        &[candidate()],
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
        rejected_candidate_indices: Vec::new(),
        assessment: assessment(),
    }
    .into_review_output(
        &ReviewTarget::Custom {
            instructions: "Compare against merge base abc123.".to_string(),
        },
        &[candidate()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(output.findings[0].pre_existing, ReviewPreExisting::True);
    assert_eq!(output.overall_correctness, "patch is correct");
}

#[test]
fn mixed_custom_scope_uses_only_non_pre_existing_findings_in_assessment() {
    let output = VerificationOutput {
        findings: vec![
            verified(ReviewPreExisting::False),
            verified(ReviewPreExisting::True),
        ],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        rejected_candidate_indices: Vec::new(),
        assessment: StageAssessment {
            verdict: "patch is incorrect".to_string(),
            explanation: "Both findings make the patch incorrect.".to_string(),
            confidence_score: 0.9,
        },
    }
    .into_review_output(
        &ReviewTarget::Custom {
            instructions: "Compare against merge base abc123.".to_string(),
        },
        &[candidate(), candidate()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert_eq!(output.overall_correctness, "patch is incorrect");
    assert_eq!(
        output.overall_explanation,
        "Verified non-pre-existing findings remain."
    );
}

#[test]
fn verification_rejects_duplicate_or_invented_candidate_indices() {
    let duplicate = verified(ReviewPreExisting::False);
    let mut invented = verified(ReviewPreExisting::False);
    invented.candidate_index = 99;
    let mut output = VerificationOutput {
        findings: vec![duplicate.clone(), duplicate, invented],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        rejected_candidate_indices: Vec::new(),
        assessment: assessment(),
    };

    let missing = output.retain_candidates(&[0]);

    assert!(output.findings.is_empty());
    assert_eq!(missing, vec![0]);
}

#[test]
fn verification_rejects_cross_bucket_candidate_conflicts() {
    let mut output = VerificationOutput {
        findings: vec![verified(ReviewPreExisting::False)],
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        rejected_candidate_indices: vec![0],
        assessment: assessment(),
    };

    let missing = output.retain_candidates(&[0]);

    assert!(output.findings.is_empty());
    assert!(output.rejected_candidate_indices.is_empty());
    assert_eq!(missing, vec![0]);
}

#[test]
fn rejected_candidate_is_not_reclassified_as_unverified() {
    let mut output = VerificationOutput {
        findings: Vec::new(),
        out_of_scope_findings: Vec::new(),
        unverified_findings: Vec::new(),
        rejected_candidate_indices: vec![0],
        assessment: assessment(),
    };

    let missing = output.retain_candidates(&[0]);
    let report = output.into_review_output(
        &ReviewTarget::WholeRepository,
        &[candidate()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );

    assert!(missing.is_empty());
    assert!(report.findings.is_empty());
    assert!(report.unverified_findings.is_empty());
}

#[test]
fn fix_output_accepts_exact_authoritative_classification() {
    let finding = classified_finding();
    let mut output = ReviewOutputEvent {
        findings: vec![finding.clone()],
        ..Default::default()
    };
    let applied = FixOutput {
        classification_updates: vec![classification_update(/*finding_index*/ 0)],
        resolution: fix_resolution(),
    }
    .apply_to(&mut output, &[0]);

    assert!(!applied.invalid_structure);
    assert_eq!(
        applied.dispositions,
        std::collections::HashMap::from([(0, FixDisposition::Fixed)])
    );
    assert_eq!(output.findings, vec![finding]);
    assert_eq!(
        output.resolution,
        Some(ReviewResolution {
            status: ReviewResolutionStatus::Complete,
            fixed_count: 1,
            rejected_count: 0,
            unresolved_count: 0,
            summary: vec!["Fixed the finding.".to_string()],
            tests: vec![ReviewTestResult {
                command: "just test".to_string(),
                status: ReviewTestStatus::Passed,
            }],
            commit_sha: None,
        })
    );
}

#[test]
fn fix_output_rejects_missing_duplicate_invented_or_changed_classifications() {
    let valid = classification_update(/*finding_index*/ 0);
    let mut changed_classification = classification_update(/*finding_index*/ 0);
    changed_classification.pre_existing = ReviewPreExisting::True;
    let mut changed_rationale = classification_update(/*finding_index*/ 0);
    changed_rationale.pre_existing_fix_rationale = None;

    for updates in [
        Vec::new(),
        vec![valid.clone(), valid],
        vec![classification_update(/*finding_index*/ 1)],
        vec![changed_classification],
        vec![changed_rationale],
    ] {
        let mut output = ReviewOutputEvent {
            findings: vec![classified_finding()],
            ..Default::default()
        };
        let applied = FixOutput {
            classification_updates: updates,
            resolution: fix_resolution(),
        }
        .apply_to(&mut output, &[0]);

        assert!(applied.invalid_structure);
    }
}

#[test]
fn assessment_normalization_covers_remaining_target_kinds() {
    for (target, pre_existing, expected) in [
        (
            ReviewTarget::WholeRepository,
            ReviewPreExisting::Undetermined,
            "patch is incorrect",
        ),
        (
            ReviewTarget::Commit {
                sha: "abc123".to_string(),
                title: None,
            },
            ReviewPreExisting::True,
            "patch is correct",
        ),
        (
            ReviewTarget::Custom {
                instructions: "Review without a comparison baseline.".to_string(),
            },
            ReviewPreExisting::Undetermined,
            "patch is incorrect",
        ),
    ] {
        let mut finding = candidate().into_review_finding();
        finding.pre_existing = pre_existing;
        let mut output = ReviewOutputEvent {
            findings: vec![finding],
            overall_correctness: "model verdict".to_string(),
            ..Default::default()
        };

        normalize_review_assessment(&target, &mut output);

        assert_eq!(output.overall_correctness, expected);
    }
}
