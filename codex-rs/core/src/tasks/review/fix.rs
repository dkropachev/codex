use std::sync::Arc;

use anyhow::Context as _;
use codex_file_system::CreateDirectoryOptions;
use codex_file_system::RemoveOptions;
use codex_git_utils::ReviewFixCommitSnapshot;
use codex_git_utils::capture_review_fix_commit_snapshot;
use codex_git_utils::resolve_review_git_directories;
use codex_prompts::review_fix_prompt;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewFinding;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewPreExisting;
use codex_protocol::protocol::ReviewResolution;
use codex_protocol::protocol::ReviewResolutionStatus;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewTestStatus;
use codex_protocol::protocol::ReviewVerification;
use codex_utils_path_uri::PathUri;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::context::ContextualUserFragment;
use crate::context::ReviewFixFindingsFragment;
use crate::review_stage_runtime::ReviewProtectedPaths;
use crate::review_stage_runtime::ReviewVerificationWriteRoot;
use crate::review_stage_runtime::ReviewWritableRoot;
use crate::session::ExecutorReviewCommandRunner;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use self::finalize::ReviewFixFinalizationInput;
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
mod scope;

pub(super) use location::sanitize_fix_locations;

const MAX_FIX_CONTEXT_BYTES: usize = 64 * 1024;

struct ReviewVerificationRootGuard {
    environment: Arc<codex_exec_server::Environment>,
    root: PathUri,
    cleaned: bool,
}

impl ReviewVerificationRootGuard {
    async fn cleanup(&mut self) -> anyhow::Result<()> {
        self.environment
            .get_filesystem()
            .remove(
                &self.root,
                RemoveOptions {
                    recursive: true,
                    force: true,
                },
                /*sandbox*/ None,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to remove review verification directory {}",
                    self.root
                )
            })?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for ReviewVerificationRootGuard {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let environment = Arc::clone(&self.environment);
        let root = self.root.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = environment
                    .get_filesystem()
                    .remove(
                        &root,
                        RemoveOptions {
                            recursive: true,
                            force: true,
                        },
                        /*sandbox*/ None,
                    )
                    .await
                {
                    tracing::warn!(%error, %root, "failed to remove abandoned review verification directory");
                }
            });
        }
    }
}

impl ReviewTask {
    pub(super) async fn run_fix_stage(
        self: &Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        output: &mut ReviewOutputEvent,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<()> {
        let provisional = eligible_finding_indices(
            &self.config.target,
            self.config.verification,
            &output.findings,
        );
        if provisional.is_empty() {
            return Ok(());
        }
        let scope = match self
            .classify_fix_scope(
                session.clone(),
                ctx.clone(),
                output,
                &provisional,
                cancellation_token.clone(),
            )
            .await
        {
            Ok(scope) => scope,
            Err(error) => {
                let mut resolution = failed_fix_resolution(provisional.len());
                resolution
                    .summary
                    .push(format!("Could not classify review fixes: {error}"));
                output.resolution = Some(resolution);
                self.remember_review_output(output).await;
                return Ok(());
            }
        };
        if scope.mutable_indices.is_empty() {
            output.resolution = Some(scope_only_resolution(&scope));
            normalize_post_fix_sections(
                &self.config.target,
                self.config.verification,
                scope.has_comparison_baseline,
                output,
            );
            normalize_review_assessment(&self.config.target, output);
            self.remember_review_output(output).await;
            return Ok(());
        }
        let (finding_items, supplied_indices) =
            match fix_finding_fragments(output, &scope.mutable_indices) {
                Ok(fragments) => fragments,
                Err(error) => {
                    let mut resolution = failed_fix_resolution(provisional.len());
                    resolution
                        .summary
                        .push(format!("Could not prepare review fixes: {error}"));
                    output.resolution = Some(resolution);
                    self.normalize_post_scope_output(&scope, output);
                    self.remember_review_output(output).await;
                    return Ok(());
                }
            };
        let mutation_omitted_count = scope
            .mutable_indices
            .len()
            .saturating_sub(supplied_indices.len());
        let snapshot = self.capture_fix_snapshot(ctx.as_ref()).await;
        if self.config.action == ReviewAction::FixAndCommit
            && let Err(error) = snapshot.as_ref()
        {
            let mut resolution = failed_fix_resolution(provisional.len());
            resolution
                .summary
                .push(format!("Could not snapshot review fix changes: {error}"));
            output.resolution = Some(resolution);
            self.normalize_post_scope_output(&scope, output);
            self.remember_review_output(output).await;
            return Ok(());
        }
        let mut protected_paths = ctx
            .extension_data
            .get::<ReviewProtectedPaths>()
            .map(|paths| paths.0.clone())
            .unwrap_or_default();
        protected_paths.push(self.config.checkout_root.join(".git")?);
        if let Ok(snapshot) = snapshot.as_ref() {
            protected_paths.extend(snapshot.protected_paths());
        }
        let mut verification_root = match self.prepare_verification_write_root(ctx.as_ref()).await {
            Ok(root) => root,
            Err(_) => {
                output.resolution = Some(failed_fix_resolution(provisional.len()));
                self.normalize_post_scope_output(&scope, output);
                self.remember_review_output(output).await;
                return Ok(());
            }
        };
        let verification_write_root = verification_root.root.clone();
        protected_paths.push(verification_write_root.clone());
        let mut seen = std::collections::HashSet::new();
        protected_paths.retain(|path| seen.insert(path.clone()));
        ctx.extension_data
            .insert(ReviewProtectedPaths(protected_paths));
        ctx.extension_data
            .insert(ReviewWritableRoot(self.config.checkout_root.clone()));
        ctx.extension_data
            .insert(ReviewVerificationWriteRoot(verification_write_root.clone()));
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
                workspace_read_root: Some(self.config.checkout_root.clone()),
                workspace_write_root: Some(self.config.checkout_root.clone()),
                include_pull_request_context: matches!(
                    self.config.target,
                    ReviewTarget::PullRequest { .. }
                ),
            },
            cancellation_token.clone(),
        )
        .await;
        let cleanup_error = verification_root
            .cleanup()
            .await
            .err()
            .map(|error| error.to_string());
        match result {
            Ok(result) => {
                let StructuredStageResult {
                    output: result,
                    evidence,
                } = result;
                let applied = result.apply_to(output, &supplied_indices);
                if matches!(self.config.target, ReviewTarget::WholeRepository) {
                    for finding in &mut output.findings {
                        finding.pre_existing = ReviewPreExisting::Undetermined;
                        finding.pre_existing_fix_rationale = None;
                    }
                }
                normalize_resolution(
                    self.config.action,
                    &supplied_indices,
                    provisional.len(),
                    scope.rejected_count,
                    scope.omitted_count.saturating_add(mutation_omitted_count),
                    &applied,
                    &evidence,
                    output,
                );
                let mut successful_file_changes =
                    evidence.resolved_file_changes(&self.config.checkout_root)?;
                if successful_file_changes
                    .iter()
                    .any(|change| fix_change_touches_root(change, &verification_write_root))
                {
                    if let Some(resolution) = output.resolution.as_mut() {
                        resolution.status = ReviewResolutionStatus::Failed;
                        resolution.unresolved_count = resolution
                            .unresolved_count
                            .saturating_add(resolution.fixed_count.max(1));
                        resolution.fixed_count = 0;
                        resolution.commit_sha = None;
                        if resolution.summary.len() < 5 {
                            resolution.summary.push(
                                "A source patch targeted the verification output directory."
                                    .to_string(),
                            );
                        }
                    }
                    successful_file_changes.clear();
                }
                if let Some(error) = cleanup_error.as_deref() {
                    if let Some(resolution) = output.resolution.as_mut() {
                        resolution.status = ReviewResolutionStatus::Partial;
                        resolution.unresolved_count = resolution
                            .unresolved_count
                            .saturating_add(resolution.fixed_count.max(1));
                        resolution.fixed_count = 0;
                        resolution.commit_sha = None;
                        if resolution.summary.len() < 5 {
                            resolution.summary.push(format!(
                                "Could not clean review verification output: {error}"
                            ));
                        }
                    }
                    successful_file_changes.clear();
                }
                normalize_post_fix_sections(
                    &self.config.target,
                    self.config.verification,
                    scope.has_comparison_baseline,
                    output,
                );
                normalize_review_assessment(&self.config.target, output);
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
                let detach_finalization = self.config.action == ReviewAction::FixAndCommit
                    && output.resolution.as_ref().is_some_and(|resolution| {
                        resolution.status == ReviewResolutionStatus::Complete
                            && resolution.fixed_count > 0
                    });
                if cancellation_token.is_cancelled() && !detach_finalization {
                    return Ok(());
                }
                let finalization = ReviewFixFinalizationInput {
                    snapshot,
                    successful_file_changes,
                    net_file_changes: evidence.net_file_changes(),
                };
                if detach_finalization {
                    self.finalize_fix_detached(session, ctx, finalization, output)
                        .await;
                } else {
                    self.finalize_fix_changes(ctx.as_ref(), finalization, output)
                        .await;
                }
                self.remember_review_output(output).await;
            }
            Err(_) => {
                let mut resolution = failed_fix_resolution(provisional.len());
                if let Some(error) = cleanup_error {
                    resolution.summary.push(format!(
                        "Could not clean review verification output: {error}"
                    ));
                }
                output.resolution = Some(resolution);
                self.normalize_post_scope_output(&scope, output);
                self.remember_review_output(output).await;
            }
        }
        Ok(())
    }

    fn normalize_post_scope_output(
        &self,
        scope: &scope::FixScopePlan,
        output: &mut ReviewOutputEvent,
    ) {
        normalize_post_fix_sections(
            &self.config.target,
            self.config.verification,
            scope.has_comparison_baseline,
            output,
        );
        normalize_review_assessment(&self.config.target, output);
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
            environment.environment.get_filesystem(),
            &self.config.checkout_root,
        )
        .await
    }

    pub(super) async fn resolve_git_protected_paths(
        &self,
        ctx: &TurnContext,
        parent_sandbox: &codex_file_system::FileSystemSandboxContext,
    ) -> anyhow::Result<Vec<PathUri>> {
        let environment = ctx
            .environments
            .primary()
            .context("review fix requires a selected environment")?;
        let runner = ExecutorReviewCommandRunner::new(
            environment.environment.get_exec_backend(),
            &ctx.config.permissions.shell_environment_policy,
        );
        let filesystem = environment.environment.get_filesystem();
        let mut paths = Vec::new();
        for path in resolve_review_git_directories(&runner, &self.config.checkout_root).await? {
            if path.starts_with(&self.config.checkout_root) {
                paths.push(path);
            } else if let Ok(path) = filesystem.canonicalize(&path, Some(parent_sandbox)).await {
                paths.push(path);
            }
        }
        let mut object_directories = paths
            .iter()
            .filter_map(|path| path.join("objects").ok())
            .collect::<std::collections::VecDeque<_>>();
        let mut seen_object_directories = std::collections::HashSet::new();
        while let Some(objects) = object_directories.pop_front() {
            if seen_object_directories.len() == 64
                || !seen_object_directories.insert(objects.clone())
            {
                continue;
            }
            let objects = if objects.starts_with(&self.config.checkout_root) {
                objects
            } else {
                let Ok(objects) = filesystem
                    .canonicalize(&objects, Some(parent_sandbox))
                    .await
                else {
                    continue;
                };
                objects
            };
            paths.push(objects.clone());
            let Ok(alternates_path) = objects.join("info/alternates") else {
                continue;
            };
            let Ok(contents) = filesystem
                .read_file(&alternates_path, Some(parent_sandbox))
                .await
            else {
                continue;
            };
            let Ok(contents) = String::from_utf8(contents) else {
                continue;
            };
            object_directories.extend(
                contents
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .filter_map(|line| objects.join(line).ok()),
            );
        }
        paths.push(self.config.checkout_root.join(".git")?);
        let mut seen = std::collections::HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        Ok(paths)
    }

    async fn prepare_verification_write_root(
        &self,
        ctx: &TurnContext,
    ) -> anyhow::Result<ReviewVerificationRootGuard> {
        let environment = ctx
            .environments
            .primary()
            .context("review fix requires a selected environment")?;
        let root = self
            .config
            .checkout_root
            .join(&format!(".codex-review-build-{}", uuid::Uuid::now_v7()))?;
        environment
            .environment
            .get_filesystem()
            .create_directory(
                &root,
                CreateDirectoryOptions { recursive: false },
                /*sandbox*/ None,
            )
            .await
            .with_context(|| format!("failed to create review verification directory {root}"))?;
        Ok(ReviewVerificationRootGuard {
            environment: Arc::clone(&environment.environment),
            root,
            cleaned: false,
        })
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

fn fix_change_touches_root(change: &codex_git_utils::ReviewFixFileChange, root: &PathUri) -> bool {
    match change {
        codex_git_utils::ReviewFixFileChange::Add { path, .. }
        | codex_git_utils::ReviewFixFileChange::Delete { path, .. } => path.starts_with(root),
        codex_git_utils::ReviewFixFileChange::Update {
            path, move_path, ..
        } => {
            path.starts_with(root)
                || move_path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(root))
        }
    }
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
    supplied_indices: &[usize],
    considered_count: usize,
    initial_rejected_count: usize,
    initial_unresolved_count: usize,
    applied: &super::output::AppliedFixOutput,
    evidence: &super::stage::ReviewStageEvidence,
    output: &mut ReviewOutputEvent,
) {
    let Some(resolution) = output.resolution.as_mut() else {
        return;
    };
    resolution.fixed_count = 0;
    resolution.rejected_count = initial_rejected_count;
    resolution.unresolved_count = initial_unresolved_count;
    for index in supplied_indices {
        match applied.dispositions.get(index) {
            Some(FixDisposition::Fixed) => resolution.fixed_count += 1,
            Some(FixDisposition::Rejected) => resolution.rejected_count += 1,
            Some(FixDisposition::Unresolved) | None => resolution.unresolved_count += 1,
        }
    }
    let verification_failure = if evidence.has_failed_file_change() {
        Some("A file change failed and may have left partial edits.")
    } else if resolution.fixed_count == 0 {
        None
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
            .saturating_add(resolution.fixed_count.max(1));
        resolution.fixed_count = 0;
        if resolution.summary.len() < 5 {
            resolution.summary.push(summary.to_string());
        }
    }
    resolution.status = if applied.invalid_structure {
        resolution.fixed_count = 0;
        resolution.rejected_count = 0;
        resolution.unresolved_count = considered_count;
        resolution.commit_sha = None;
        if resolution.summary.len() < 5 {
            resolution
                .summary
                .push("The fix result did not match the supplied findings.".to_string());
        }
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

fn normalize_post_fix_sections(
    target: &ReviewTarget,
    verification: ReviewVerification,
    has_comparison_baseline: bool,
    output: &mut ReviewOutputEvent,
) {
    match target {
        ReviewTarget::PullRequest { .. } if verification == ReviewVerification::DoubleCheck => {
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
        ReviewTarget::Custom { .. } if has_comparison_baseline => {
            let mut findings = Vec::new();
            for finding in output.findings.drain(..) {
                if finding.pre_existing == ReviewPreExisting::Undetermined {
                    output.unverified_findings.push(finding);
                } else {
                    findings.push(finding);
                }
            }
            output.findings = findings;
        }
        ReviewTarget::UncommittedChanges
        | ReviewTarget::PullRequest { .. }
        | ReviewTarget::Custom { .. }
        | ReviewTarget::BaseBranch { .. }
        | ReviewTarget::Commit { .. } => {}
    }
}

fn scope_only_resolution(scope: &scope::FixScopePlan) -> ReviewResolution {
    let unresolved_count = scope.omitted_count;
    let mut summary = Vec::new();
    if scope.rejected_count > 0 {
        summary.push("Rejected findings that were not valid.".to_string());
    }
    if scope.report_only_count > 0 {
        summary.push("Kept report-only findings unchanged.".to_string());
    }
    if scope.omitted_count > 0 {
        summary.push("Some findings exceeded the fix context limit.".to_string());
    }
    ReviewResolution {
        status: if unresolved_count > 0 {
            ReviewResolutionStatus::Partial
        } else {
            ReviewResolutionStatus::Complete
        },
        fixed_count: 0,
        rejected_count: scope.rejected_count,
        unresolved_count,
        summary,
        tests: Vec::new(),
        commit_sha: None,
    }
}

#[cfg(test)]
#[path = "fix_tests.rs"]
mod tests;
