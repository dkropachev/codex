use std::sync::Arc;

use codex_git_utils::ReviewFixCommitOutcome;
use codex_git_utils::ReviewFixCommitSnapshot;
use codex_git_utils::ReviewFixFileChange;
use codex_git_utils::commit_review_fixes;
use codex_git_utils::review_fix_snapshot_has_changes;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewResolution;
use codex_protocol::protocol::ReviewResolutionStatus;

use crate::session::ExecutorReviewCommandRunner;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

use super::ReviewTask;

impl ReviewTask {
    pub(super) async fn finalize_fix_and_commit(
        self: &Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        snapshot: anyhow::Result<ReviewFixCommitSnapshot>,
        successful_file_changes: Vec<ReviewFixFileChange>,
        output: &mut ReviewOutputEvent,
    ) {
        let guard = self.fix_finalization.begin();
        let task = Arc::clone(self);
        let mut final_output = output.clone();
        let handle = tokio::spawn(async move {
            let _guard = guard;
            task.finalize_fix_changes(
                ctx.as_ref(),
                snapshot,
                &successful_file_changes,
                &mut final_output,
            )
            .await;
            task.remember_review_output(&final_output).await;
            task.exit_once(session, Some(final_output.clone()), ctx)
                .await;
            final_output
        });
        match handle.await {
            Ok(final_output) => *output = final_output,
            Err(error) => {
                if let Some(resolution) = output.resolution.as_mut() {
                    mark_fix_unresolved(
                        resolution,
                        format!("Review fix finalization failed: {error}"),
                    );
                }
            }
        }
    }

    pub(super) async fn finalize_fix_changes(
        &self,
        ctx: &TurnContext,
        snapshot: anyhow::Result<ReviewFixCommitSnapshot>,
        successful_file_changes: &[ReviewFixFileChange],
        output: &mut ReviewOutputEvent,
    ) {
        let Some(resolution) = output.resolution.as_mut() else {
            return;
        };
        resolution.commit_sha = None;
        if resolution.fixed_count == 0 {
            if !successful_file_changes.is_empty() {
                resolution.status = ReviewResolutionStatus::Partial;
                if resolution.unresolved_count == 0 {
                    if resolution.rejected_count > 0 {
                        resolution.rejected_count -= 1;
                    }
                    resolution.unresolved_count = 1;
                }
                if resolution.summary.len() < 5 {
                    resolution.summary.push(
                        "The fix stage changed files without accepting a fixed finding."
                            .to_string(),
                    );
                }
            }
            return;
        }
        let snapshot = match snapshot {
            Ok(snapshot) => snapshot,
            Err(_)
                if self.config.action == ReviewAction::Fix
                    && !successful_file_changes.is_empty() =>
            {
                return;
            }
            Err(error) => {
                mark_fix_unresolved(
                    resolution,
                    format!("Could not snapshot review fix changes: {error}"),
                );
                return;
            }
        };
        let Some(environment) = ctx.environments.primary() else {
            mark_fix_unresolved(
                resolution,
                "Could not access the review fix environment.".to_string(),
            );
            return;
        };
        let runner = ExecutorReviewCommandRunner::new(
            environment.environment.get_exec_backend(),
            &ctx.config.permissions.shell_environment_policy,
        );
        let filesystem = environment.environment.get_filesystem();
        match review_fix_snapshot_has_changes(
            &runner,
            filesystem.as_ref(),
            &snapshot,
            successful_file_changes,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                mark_fix_unresolved(
                    resolution,
                    "The fix stage reported fixes but changed no files.".to_string(),
                );
                return;
            }
            Err(error) => {
                mark_fix_unresolved(
                    resolution,
                    format!("Could not verify review fix changes: {error}"),
                );
                return;
            }
        }
        if self.config.action != ReviewAction::FixAndCommit
            || resolution.status != ReviewResolutionStatus::Complete
        {
            return;
        }
        match commit_review_fixes(
            Arc::new(runner),
            filesystem,
            &snapshot,
            successful_file_changes,
            "fix: address review findings",
        )
        .await
        {
            Ok(ReviewFixCommitOutcome::Committed { commit_sha }) => {
                resolution.commit_sha = Some(commit_sha);
            }
            Ok(ReviewFixCommitOutcome::NoChanges) => mark_fix_unresolved(
                resolution,
                "No verified review fix changes were available to commit.".to_string(),
            ),
            Err(error) => mark_fix_unresolved(
                resolution,
                format!("Could not create the review fix commit: {error}"),
            ),
        }
    }
}

fn mark_fix_unresolved(resolution: &mut ReviewResolution, summary: String) {
    resolution.status = ReviewResolutionStatus::Partial;
    resolution.unresolved_count = resolution
        .unresolved_count
        .saturating_add(resolution.fixed_count);
    resolution.fixed_count = 0;
    resolution.commit_sha = None;
    if resolution.summary.len() < 5 {
        resolution.summary.push(summary);
    }
}
