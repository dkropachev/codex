use std::sync::Arc;

use anyhow::Context as _;
use codex_git_utils::ReviewFixCommitSnapshot;
use codex_git_utils::capture_review_fix_commit_snapshot;
use codex_git_utils::resolve_review_git_directories;
use codex_prompts::review_fix_prompt;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewResolutionStatus;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewTestStatus;
use codex_protocol::protocol::ReviewVerification;
use codex_utils_path_uri::PathUri;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::codex_delegate::ReviewProtectedPaths;
use crate::context::ContextualUserFragment;
use crate::context::ReviewFixFindingsFragment;
use crate::session::ExecutorReviewCommandRunner;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::ReviewTask;
use super::StructuredStageResult;
use super::output::FixDisposition;
use super::output::FixOutput;
use super::output::failed_fix_resolution;
use super::output::normalize_review_assessment;
use super::run_structured_stage;
use super::schema::fix_schema;
use super::stage::ReviewStageRequest;
use super::stage::StagePermissions;
use super::stage_control_prompt;
use super::target_context_item;

mod finalize;
mod location;

pub(super) use location::sanitize_fix_locations;

const MAX_FIX_CONTEXT_BYTES: usize = 64 * 1024;

impl ReviewTask {
    pub(super) async fn run_fix_stage(
        self: &Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        output: &mut ReviewOutputEvent,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let eligible = eligible_finding_indices(
            &self.config.target,
            self.config.verification,
            &output.findings,
        );
        if eligible.is_empty() {
            return Ok(());
        }
        let (finding_items, supplied_indices) = fix_finding_fragments(output, &eligible)?;
        let omitted_count = eligible.len().saturating_sub(supplied_indices.len());
        let snapshot = self.capture_fix_snapshot(ctx.as_ref()).await;
        if snapshot.is_ok() {
            let protected_paths = self.resolve_git_protected_paths(ctx.as_ref()).await?;
            ctx.extension_data
                .insert(ReviewProtectedPaths(protected_paths));
        }
        let mut context_items = vec![target_context_item(&self.config.target_instructions)?];
        context_items.extend(finding_items);
        let result = run_structured_stage::<FixOutput>(
            session.clone(),
            ctx.clone(),
            ReviewStageRequest {
                model: self.config.coding_model.clone(),
                system_prompt: review_fix_prompt(self.config.action),
                context_items,
                user_prompt: stage_control_prompt("Revalidate and resolve the supplied findings.")?,
                output_schema: fix_schema(),
                permissions: StagePermissions::WorkspaceWrite,
                include_pull_request_context: false,
            },
            cancellation_token.clone(),
        )
        .await;
        match result {
            Ok(result) => {
                let StructuredStageResult {
                    output: result,
                    evidence,
                } = result;
                let applied = result.apply_to(output, &supplied_indices);
                normalize_resolution(
                    self.config.action,
                    &self.config.target,
                    &supplied_indices,
                    omitted_count,
                    &applied,
                    &evidence,
                    output,
                );
                normalize_post_fix_sections(&self.config.target, output);
                normalize_review_assessment(&self.config.target, output);
                let successful_file_changes =
                    evidence.resolved_file_changes(&self.config.checkout_root)?;
                let mut finalizing_output = output.clone();
                if let Some(resolution) = finalizing_output.resolution.as_mut()
                    && resolution.fixed_count > 0
                {
                    resolution.status = ReviewResolutionStatus::Partial;
                    resolution.unresolved_count = resolution
                        .unresolved_count
                        .saturating_add(resolution.fixed_count);
                    resolution.fixed_count = 0;
                    resolution.commit_sha = None;
                    if resolution.summary.len() < 5 {
                        resolution
                            .summary
                            .push("Final verification was interrupted.".to_string());
                    }
                }
                self.remember_review_output(&finalizing_output).await;
                if cancellation_token.is_cancelled() {
                    return Ok(());
                }
                if self.config.action == ReviewAction::FixAndCommit
                    && output.resolution.as_ref().is_some_and(|resolution| {
                        resolution.status == ReviewResolutionStatus::Complete
                            && resolution.fixed_count > 0
                    })
                {
                    self.finalize_fix_and_commit(
                        session,
                        ctx,
                        snapshot,
                        successful_file_changes,
                        output,
                    )
                    .await;
                } else {
                    self.finalize_fix_changes(
                        ctx.as_ref(),
                        snapshot,
                        &successful_file_changes,
                        output,
                    )
                    .await;
                }
                self.remember_review_output(output).await;
            }
            Err(_) => {
                output.resolution = Some(failed_fix_resolution(eligible.len()));
                self.remember_review_output(output).await;
            }
        }
        Ok(())
    }

    async fn capture_fix_snapshot(
        &self,
        ctx: &TurnContext,
    ) -> anyhow::Result<ReviewFixCommitSnapshot> {
        let environment = ctx
            .environments
            .primary()
            .context("review fix requires a selected environment")?;
        let runner = ExecutorReviewCommandRunner::new(
            environment.environment.get_exec_backend(),
            &ctx.config.permissions.shell_environment_policy,
        );
        capture_review_fix_commit_snapshot(
            &runner,
            environment.environment.get_filesystem().as_ref(),
            &self.config.checkout_root,
        )
        .await
    }

    async fn resolve_git_protected_paths(&self, ctx: &TurnContext) -> anyhow::Result<Vec<PathUri>> {
        let environment = ctx
            .environments
            .primary()
            .context("review fix requires a selected environment")?;
        let runner = ExecutorReviewCommandRunner::new(
            environment.environment.get_exec_backend(),
            &ctx.config.permissions.shell_environment_policy,
        );
        resolve_review_git_directories(&runner, &self.config.checkout_root).await
    }
}

fn eligible_finding_indices(
    target: &ReviewTarget,
    verification: ReviewVerification,
    findings: &[ReviewFinding],
) -> Vec<usize> {
    findings
        .iter()
        .enumerate()
        .filter_map(|(index, finding)| {
            let eligible = match target {
                ReviewTarget::WholeRepository | ReviewTarget::Custom { .. } => {
                    finding.pre_existing != ReviewPreExisting::True
                }
                ReviewTarget::PullRequest { .. }
                | ReviewTarget::UncommittedChanges
                | ReviewTarget::BaseBranch { .. }
                | ReviewTarget::Commit { .. } => {
                    finding.pre_existing == ReviewPreExisting::False
                        || (verification == ReviewVerification::SinglePass
                            && finding.pre_existing == ReviewPreExisting::Undetermined)
                }
            };
            eligible.then_some(index)
        })
        .collect()
}

fn fix_finding_fragments(
    output: &ReviewOutputEvent,
    eligible: &[usize],
) -> anyhow::Result<(Vec<ResponseItem>, Vec<usize>)> {
    let mut fragments = Vec::new();
    let mut supplied = Vec::new();
    let mut total_bytes = 0;
    for index in eligible {
        let Some(finding) = output.findings.get(*index) else {
            continue;
        };
        let mut body_bytes = 6 * 1024;
        let fragment = loop {
            let body = codex_utils_string::take_bytes_at_char_boundary(&finding.body, body_bytes);
            let pre_existing = match finding.pre_existing {
                ReviewPreExisting::True => "true",
                ReviewPreExisting::False => "false",
                ReviewPreExisting::Undetermined => "undetermined",
            };
            let json = json!({
                "findings": [{
                    "findingIndex": index,
                    "title": finding.title,
                    "body": body,
                    "confidenceScore": finding.confidence_score,
                    "priority": finding.priority,
                    "codeLocation": {
                        "absoluteFilePath": finding.code_location.absolute_file_path.display().to_string(),
                        "lineRange": {
                            "start": finding.code_location.line_range.start,
                            "end": finding.code_location.line_range.end
                        }
                    },
                    "preExisting": pre_existing,
                    "preExistingFixRationale": finding.pre_existing_fix_rationale
                }]
            })
            .to_string();
            match ReviewFixFindingsFragment::new(json) {
                Ok(fragment) => break fragment,
                Err(_) if body_bytes > 128 => body_bytes /= 2,
                Err(error) => return Err(error.into()),
            }
        };
        let fragment_bytes = fragment.render().len();
        if fragment_bytes > MAX_FIX_CONTEXT_BYTES.saturating_sub(total_bytes) {
            continue;
        }
        total_bytes += fragment_bytes;
        fragments.push(ContextualUserFragment::into(fragment));
        supplied.push(*index);
    }
    Ok((fragments, supplied))
}

fn normalize_resolution(
    action: ReviewAction,
    target: &ReviewTarget,
    supplied_indices: &[usize],
    omitted_count: usize,
    applied: &super::output::AppliedFixOutput,
    evidence: &super::stage::ReviewStageEvidence,
    output: &mut ReviewOutputEvent,
) {
    let Some(resolution) = output.resolution.as_mut() else {
        return;
    };
    resolution.fixed_count = 0;
    resolution.rejected_count = 0;
    resolution.unresolved_count = omitted_count;
    for index in supplied_indices {
        let disposition = applied.dispositions.get(index);
        let is_report_only = output
            .findings
            .get(*index)
            .is_none_or(|finding| match target {
                ReviewTarget::WholeRepository | ReviewTarget::Custom { .. } => {
                    finding.pre_existing == ReviewPreExisting::True
                }
                ReviewTarget::PullRequest { .. }
                | ReviewTarget::UncommittedChanges
                | ReviewTarget::BaseBranch { .. }
                | ReviewTarget::Commit { .. } => finding.pre_existing != ReviewPreExisting::False,
            });
        match disposition {
            Some(FixDisposition::Fixed) if !is_report_only => {
                resolution.fixed_count += 1;
            }
            Some(FixDisposition::Rejected) if !is_report_only => {
                resolution.rejected_count += 1;
            }
            Some(FixDisposition::Unresolved)
            | Some(FixDisposition::Fixed | FixDisposition::Rejected)
            | None => {
                resolution.unresolved_count += 1;
            }
        }
    }
    let verification_failure = if resolution.fixed_count == 0 {
        None
    } else if evidence.has_potentially_mutating_command() {
        Some("A potentially mutating command ran during fix verification.")
    } else if resolution.tests.is_empty() {
        Some("Fixes had no reported verification commands.")
    } else if resolution
        .tests
        .iter()
        .any(|test| test.status != ReviewTestStatus::Passed)
    {
        Some("One or more reported verification commands did not pass.")
    } else if resolution
        .tests
        .iter()
        .any(|test| !evidence.observed_successful_command_after_last_mutation(&test.command))
    {
        Some("A reported passing test lacked successful command evidence.")
    } else {
        None
    };
    if let Some(summary) = verification_failure {
        resolution.unresolved_count = resolution
            .unresolved_count
            .saturating_add(resolution.fixed_count);
        resolution.fixed_count = 0;
        if resolution.summary.len() < 5 {
            resolution.summary.push(summary.to_string());
        }
    }
    resolution.status = if applied.invalid_structure {
        ReviewResolutionStatus::Failed
    } else if resolution.unresolved_count > 0 || verification_failure.is_some() {
        ReviewResolutionStatus::Partial
    } else {
        ReviewResolutionStatus::Complete
    };
    if action == ReviewAction::Fix {
        resolution.commit_sha = None;
    }
    if resolution.status != ReviewResolutionStatus::Complete {
        resolution.commit_sha = None;
    }
    resolution.summary.truncate(5);
}

fn normalize_post_fix_sections(target: &ReviewTarget, output: &mut ReviewOutputEvent) {
    match target {
        ReviewTarget::PullRequest { .. } => {
            let mut findings = Vec::new();
            for finding in output.findings.drain(..) {
                match finding.pre_existing {
                    ReviewPreExisting::False => findings.push(finding),
                    ReviewPreExisting::True => output.out_of_scope_findings.push(finding),
                    ReviewPreExisting::Undetermined => output.unverified_findings.push(finding),
                }
            }
            output.findings = findings;
        }
        ReviewTarget::WholeRepository => {
            for finding in &mut output.findings {
                finding.pre_existing = ReviewPreExisting::Undetermined;
                finding.pre_existing_fix_rationale = None;
            }
        }
        ReviewTarget::Custom { .. } => {}
        ReviewTarget::UncommittedChanges
        | ReviewTarget::BaseBranch { .. }
        | ReviewTarget::Commit { .. } => {}
    }
}

#[cfg(test)]
#[path = "fix_tests.rs"]
mod tests;
