use std::collections::HashSet;
use std::sync::Arc;

use anyhow::bail;
use codex_prompts::REVIEW_FIX_SCOPE_PROMPT;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewTarget;
use tokio_util::sync::CancellationToken;

use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::super::ReviewTask;
use super::super::run_structured_stage;
use super::super::schema::fix_scope_schema;
use super::super::stage::ReviewStageRequest;
use super::super::stage::StagePermissions;
use super::super::stage_control_prompt;
use super::super::target_context_item;
use super::fix_finding_fragments;
use crate::tasks::review::output::FixScopeClassification;
use crate::tasks::review::output::FixScopeOutput;
use crate::tasks::review::output::FixScopeValidity;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixBaselineKind {
    Required,
    Custom,
    Absent,
}

#[derive(Debug)]
pub(super) struct FixScopePlan {
    pub(super) mutable_indices: Vec<usize>,
    pub(super) omitted_count: usize,
    pub(super) rejected_count: usize,
    pub(super) report_only_count: usize,
    pub(super) has_comparison_baseline: bool,
}

impl ReviewTask {
    pub(super) async fn classify_fix_scope(
        &self,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        output: &mut ReviewOutputEvent,
        provisional_indices: &[usize],
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<FixScopePlan> {
        let (finding_items, supplied_indices) = fix_finding_fragments(output, provisional_indices)?;
        let omitted_count = provisional_indices
            .len()
            .saturating_sub(supplied_indices.len());
        if supplied_indices.is_empty() {
            return Ok(FixScopePlan {
                mutable_indices: Vec::new(),
                omitted_count,
                rejected_count: 0,
                report_only_count: 0,
                has_comparison_baseline: false,
            });
        }

        let mut context_items = vec![target_context_item(&self.config.target_instructions)?];
        context_items.extend(finding_items);
        let scope = run_structured_stage::<FixScopeOutput>(
            session,
            ctx,
            ReviewStageRequest {
                model: self.config.coding_model.clone(),
                system_prompt: REVIEW_FIX_SCOPE_PROMPT.to_string(),
                context_items,
                user_prompt: stage_control_prompt(
                    "Classify the supplied findings before any mutation stage.",
                )?,
                output_schema: fix_scope_schema(),
                permissions: StagePermissions::ReadOnly,
                workspace_read_root: Some(self.config.checkout_root.clone()),
                workspace_write_root: None,
                include_pull_request_context: matches!(
                    self.config.target,
                    ReviewTarget::PullRequest { .. }
                ),
            },
            cancellation_token,
        )
        .await?
        .output;

        apply_scope_output(
            &self.config.target,
            output,
            &supplied_indices,
            scope,
            omitted_count,
        )
    }
}

fn apply_scope_output(
    target: &ReviewTarget,
    output: &mut ReviewOutputEvent,
    supplied_indices: &[usize],
    scope: FixScopeOutput,
    omitted_count: usize,
) -> anyhow::Result<FixScopePlan> {
    let baseline_kind = match target {
        ReviewTarget::PullRequest { .. }
        | ReviewTarget::UncommittedChanges
        | ReviewTarget::BaseBranch { .. }
        | ReviewTarget::Commit { .. } => FixBaselineKind::Required,
        ReviewTarget::Custom { .. } => FixBaselineKind::Custom,
        ReviewTarget::WholeRepository => FixBaselineKind::Absent,
    };
    match baseline_kind {
        FixBaselineKind::Required if !scope.has_comparison_baseline => {
            bail!("the selected review target has a comparison baseline");
        }
        FixBaselineKind::Absent if scope.has_comparison_baseline => {
            bail!("whole-repository review has no comparison baseline");
        }
        FixBaselineKind::Required | FixBaselineKind::Custom | FixBaselineKind::Absent => {}
    }

    let expected = supplied_indices.iter().copied().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for classification in &scope.classifications {
        if !expected.contains(&classification.finding_index)
            || !seen.insert(classification.finding_index)
        {
            bail!("scope classifications must contain each supplied findingIndex exactly once");
        }
        validate_classification(scope.has_comparison_baseline, classification)?;
    }
    if seen != expected {
        bail!("scope classifications must contain each supplied findingIndex exactly once");
    }

    let mut mutable_indices = Vec::new();
    let mut rejected_count = 0;
    let mut report_only_count = 0;
    for classification in scope.classifications {
        let Some(finding) = output.findings.get_mut(classification.finding_index) else {
            bail!("scope classification referenced a missing finding");
        };
        finding.pre_existing = classification.pre_existing;
        finding.pre_existing_fix_rationale = classification.pre_existing_fix_rationale;
        match classification.validity {
            FixScopeValidity::Rejected => rejected_count += 1,
            FixScopeValidity::Valid
                if finding_is_eligible(scope.has_comparison_baseline, finding) =>
            {
                mutable_indices.push(classification.finding_index);
            }
            FixScopeValidity::Valid => report_only_count += 1,
        }
    }
    mutable_indices.sort_unstable();

    Ok(FixScopePlan {
        mutable_indices,
        omitted_count,
        rejected_count,
        report_only_count,
        has_comparison_baseline: scope.has_comparison_baseline,
    })
}

fn validate_classification(
    has_comparison_baseline: bool,
    classification: &FixScopeClassification,
) -> anyhow::Result<()> {
    if !has_comparison_baseline
        && (classification.pre_existing != ReviewPreExisting::Undetermined
            || classification.pre_existing_fix_rationale.is_some())
    {
        bail!("baseline-free findings must use preExisting=undetermined and a null rationale");
    }
    if classification.pre_existing != ReviewPreExisting::True
        && classification.pre_existing_fix_rationale.is_some()
    {
        bail!("only pre-existing findings may include a fix rationale");
    }
    Ok(())
}

fn finding_is_eligible(has_comparison_baseline: bool, finding: &ReviewFinding) -> bool {
    if has_comparison_baseline {
        finding.pre_existing == ReviewPreExisting::False
    } else {
        finding.pre_existing == ReviewPreExisting::Undetermined
    }
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
