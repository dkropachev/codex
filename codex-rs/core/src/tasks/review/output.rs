use std::collections::HashSet;
use std::path::PathBuf;

use codex_protocol::protocol::ReviewCodeLocation;
use codex_protocol::protocol::ReviewExternalReference;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewLineRange;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewReference;
use codex_protocol::protocol::ReviewResolution;
use codex_protocol::protocol::ReviewResolutionStatus;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewTestResult;
use codex_protocol::protocol::ReviewTestStatus;
use serde::Deserialize;
use serde::Serialize;

use crate::context::bounded_candidates as bounded_candidate_fragment;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DiscoveryOutput {
    pub(super) candidates: Vec<StageFinding>,
    pub(super) assessment: StageAssessment,
    pub(super) review_context: Vec<StageCodeLocation>,
    pub(super) external_references: Vec<ReviewExternalReference>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct VerificationOutput {
    pub(super) findings: Vec<VerifiedFinding>,
    pub(super) out_of_scope_findings: Vec<VerifiedFinding>,
    pub(super) unverified_findings: Vec<VerifiedFinding>,
    pub(super) rejected_candidate_indices: Vec<usize>,
    pub(super) assessment: StageAssessment,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FixScopeOutput {
    pub(super) has_comparison_baseline: bool,
    pub(super) classifications: Vec<FixScopeClassification>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FixScopeClassification {
    pub(super) finding_index: usize,
    pub(super) pre_existing: ReviewPreExisting,
    pub(super) pre_existing_fix_rationale: Option<String>,
    pub(super) validity: FixScopeValidity,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FixScopeValidity {
    Valid,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StageFinding {
    pub(super) title: String,
    pub(super) body: String,
    pub(super) confidence_score: f32,
    pub(super) priority: i32,
    pub(super) code_location: StageCodeLocation,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct VerifiedFinding {
    pub(super) candidate_index: usize,
    pub(super) pre_existing: ReviewPreExisting,
    pub(super) pre_existing_fix_rationale: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StageCodeLocation {
    pub(super) absolute_file_path: String,
    pub(super) line_range: ReviewLineRange,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StageAssessment {
    pub(super) verdict: String,
    pub(super) explanation: String,
    pub(super) confidence_score: f32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FixOutput {
    pub(super) classification_updates: Vec<ClassificationUpdate>,
    pub(super) resolution: FixResolution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ClassificationUpdate {
    pub(super) finding_index: usize,
    pub(super) pre_existing: ReviewPreExisting,
    pub(super) pre_existing_fix_rationale: Option<String>,
    pub(super) disposition: FixDisposition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum FixDisposition {
    Fixed,
    Rejected,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FixResolution {
    pub(super) status: ReviewResolutionStatus,
    pub(super) fixed_count: usize,
    pub(super) rejected_count: usize,
    pub(super) unresolved_count: usize,
    pub(super) summary: Vec<String>,
    pub(super) tests: Vec<FixTestResult>,
    pub(super) commit_sha: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct FixTestResult {
    pub(super) command: String,
    pub(super) status: ReviewTestStatus,
}

pub(super) struct BoundedCandidates {
    pub(super) json: String,
    pub(super) omitted: Vec<ReviewFinding>,
    pub(super) included_indices: Vec<usize>,
}

impl DiscoveryOutput {
    pub(super) fn single_pass_output(self) -> ReviewOutputEvent {
        ReviewOutputEvent {
            findings: self
                .candidates
                .into_iter()
                .map(StageFinding::into_review_finding)
                .collect(),
            overall_correctness: self.assessment.verdict,
            overall_explanation: self.assessment.explanation,
            overall_confidence_score: self.assessment.confidence_score,
            external_references: self.external_references,
            ..Default::default()
        }
    }

    pub(super) fn bounded_candidates(&self) -> BoundedCandidates {
        let mut included = Vec::new();
        let mut omitted = Vec::new();
        let mut included_indices = Vec::new();
        for (candidate_index, candidate) in self.candidates.iter().enumerate() {
            let Ok(mut candidate_value) = serde_json::to_value(candidate) else {
                omitted.push(candidate.clone().into_review_finding());
                continue;
            };
            let Some(candidate_object) = candidate_value.as_object_mut() else {
                omitted.push(candidate.clone().into_review_finding());
                continue;
            };
            candidate_object.insert("candidateIndex".to_string(), candidate_index.into());
            included.push(candidate_value);
            let Ok(json) = serde_json::to_string(&included) else {
                included.pop();
                omitted.push(candidate.clone().into_review_finding());
                continue;
            };
            if bounded_candidate_fragment(&json).was_truncated() {
                included.pop();
                omitted.push(candidate.clone().into_review_finding());
            } else {
                included_indices.push(candidate_index);
            }
        }
        let json = serde_json::to_string(&included).unwrap_or_else(|_| "[]".to_string());
        BoundedCandidates {
            json,
            omitted,
            included_indices,
        }
    }
}

impl VerificationOutput {
    pub(super) fn retain_candidates(&mut self, candidate_indices: &[usize]) -> Vec<usize> {
        let mut remaining = candidate_indices.iter().copied().collect::<HashSet<_>>();
        for findings in [
            &mut self.findings,
            &mut self.out_of_scope_findings,
            &mut self.unverified_findings,
        ] {
            findings.retain(|finding| remaining.remove(&finding.candidate_index));
        }
        self.rejected_candidate_indices
            .retain(|index| remaining.remove(index));
        candidate_indices
            .iter()
            .copied()
            .filter(|index| remaining.contains(index))
            .collect()
    }

    pub(super) fn into_review_output(
        self,
        target: &ReviewTarget,
        candidates: &[StageFinding],
        references: Vec<ReviewReference>,
        external_references: Vec<ReviewExternalReference>,
        mut omitted_candidates: Vec<ReviewFinding>,
    ) -> ReviewOutputEvent {
        let mut findings = Vec::new();
        let mut out_of_scope_findings = Vec::new();
        let mut unverified_findings = self
            .unverified_findings
            .into_iter()
            .filter_map(|finding| finding.into_review_finding(candidates))
            .collect::<Vec<_>>();
        unverified_findings.append(&mut omitted_candidates);

        let mut classified = self
            .findings
            .into_iter()
            .chain(self.out_of_scope_findings)
            .filter_map(|finding| finding.into_review_finding(candidates))
            .collect::<Vec<_>>();

        match target {
            ReviewTarget::PullRequest { .. } => {
                for finding in classified {
                    match finding.pre_existing {
                        ReviewPreExisting::False => findings.push(finding),
                        ReviewPreExisting::True => out_of_scope_findings.push(finding),
                        ReviewPreExisting::Undetermined => unverified_findings.push(finding),
                    }
                }
            }
            ReviewTarget::WholeRepository => {
                for finding in &mut classified {
                    finding.pre_existing = ReviewPreExisting::Undetermined;
                    finding.pre_existing_fix_rationale = None;
                }
                findings = classified;
            }
            ReviewTarget::Custom { .. }
            | ReviewTarget::UncommittedChanges
            | ReviewTarget::BaseBranch { .. }
            | ReviewTarget::Commit { .. } => findings = classified,
        }

        let mut output = ReviewOutputEvent {
            findings,
            overall_correctness: self.assessment.verdict,
            overall_explanation: self.assessment.explanation,
            overall_confidence_score: self.assessment.confidence_score,
            out_of_scope_findings,
            unverified_findings,
            references,
            external_references,
            resolution: None,
        };
        normalize_review_assessment(target, &mut output);
        output
    }
}

pub(super) fn normalize_review_assessment(target: &ReviewTarget, output: &mut ReviewOutputEvent) {
    let has_in_scope_bug = match target {
        ReviewTarget::WholeRepository => !output.findings.is_empty(),
        ReviewTarget::Custom { .. } => output
            .findings
            .iter()
            .any(|finding| finding.pre_existing != ReviewPreExisting::True),
        ReviewTarget::PullRequest { .. }
        | ReviewTarget::UncommittedChanges
        | ReviewTarget::BaseBranch { .. }
        | ReviewTarget::Commit { .. } => output
            .findings
            .iter()
            .any(|finding| finding.pre_existing == ReviewPreExisting::False),
    };
    let verdict = if has_in_scope_bug {
        "patch is incorrect"
    } else if !output.unverified_findings.is_empty()
        || output
            .findings
            .iter()
            .any(|finding| finding.pre_existing == ReviewPreExisting::Undetermined)
    {
        "uncertain"
    } else {
        "patch is correct"
    };
    let has_report_only_finding = output
        .findings
        .iter()
        .chain(&output.out_of_scope_findings)
        .any(|finding| finding.pre_existing == ReviewPreExisting::True);
    if output.overall_correctness != verdict || has_report_only_finding {
        output.overall_explanation = match verdict {
            "patch is incorrect" => "Verified non-pre-existing findings remain.",
            "uncertain" => "Some findings could not be verified or scoped conclusively.",
            "patch is correct" => "No verified non-pre-existing findings remain.",
            _ => unreachable!("review verdict is selected from fixed values"),
        }
        .to_string();
        output.overall_correctness = verdict.to_string();
    }
}

impl FixOutput {
    pub(super) fn apply_to(
        self,
        output: &mut ReviewOutputEvent,
        allowed_indices: &[usize],
    ) -> AppliedFixOutput {
        let mut invalid_structure = false;
        let mut updated_indices = HashSet::new();
        let mut dispositions = std::collections::HashMap::new();
        for update in self.classification_updates {
            if allowed_indices.contains(&update.finding_index)
                && updated_indices.insert(update.finding_index)
                && let Some(finding) = output.findings.get(update.finding_index)
            {
                invalid_structure |= update.pre_existing != finding.pre_existing
                    || update.pre_existing_fix_rationale != finding.pre_existing_fix_rationale;
                dispositions.insert(update.finding_index, update.disposition);
            } else {
                invalid_structure = true;
            }
        }
        invalid_structure |= allowed_indices
            .iter()
            .any(|index| !updated_indices.contains(index));
        output.resolution = Some(self.resolution.into_review_resolution());
        AppliedFixOutput {
            dispositions,
            invalid_structure,
        }
    }
}

pub(super) struct AppliedFixOutput {
    pub(super) dispositions: std::collections::HashMap<usize, FixDisposition>,
    pub(super) invalid_structure: bool,
}

impl StageFinding {
    pub(super) fn into_review_finding(self) -> ReviewFinding {
        ReviewFinding {
            title: self.title,
            body: self.body,
            confidence_score: self.confidence_score,
            priority: self.priority,
            code_location: self.code_location.into_review_code_location(),
            pre_existing: ReviewPreExisting::Undetermined,
            pre_existing_fix_rationale: None,
        }
    }
}

impl VerifiedFinding {
    fn into_review_finding(self, candidates: &[StageFinding]) -> Option<ReviewFinding> {
        let mut finding = candidates
            .get(self.candidate_index)?
            .clone()
            .into_review_finding();
        finding.pre_existing = self.pre_existing;
        finding.pre_existing_fix_rationale = self.pre_existing_fix_rationale;
        Some(finding)
    }
}

impl StageCodeLocation {
    fn into_review_code_location(self) -> ReviewCodeLocation {
        ReviewCodeLocation {
            absolute_file_path: PathBuf::from(self.absolute_file_path),
            line_range: self.line_range,
        }
    }
}

impl FixResolution {
    fn into_review_resolution(self) -> ReviewResolution {
        ReviewResolution {
            status: self.status,
            fixed_count: self.fixed_count,
            rejected_count: self.rejected_count,
            unresolved_count: self.unresolved_count,
            summary: self.summary.into_iter().take(5).collect(),
            tests: self
                .tests
                .into_iter()
                .map(|test| ReviewTestResult {
                    command: test.command,
                    status: test.status,
                })
                .collect(),
            commit_sha: self.commit_sha,
        }
    }
}

pub(super) fn failed_fix_resolution(unresolved_count: usize) -> ReviewResolution {
    ReviewResolution {
        status: ReviewResolutionStatus::Failed,
        fixed_count: 0,
        rejected_count: 0,
        unresolved_count,
        summary: vec!["Fix stage returned invalid structured output.".to_string()],
        tests: Vec::new(),
        commit_sha: None,
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
