use std::sync::Arc;

use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;

use crate::context::PullRequestContext;
use crate::session::TurnInput;
use crate::session::review_command_runner::ExecutorReviewCommandRunner;
use crate::session::session::Session;
use crate::tasks::ReviewTask;

pub(super) async fn review(sess: &Arc<Session>, sub_id: String, review_request: ReviewRequest) {
    let scope_requires_executor = matches!(
        &review_request.target,
        ReviewTarget::BaseBranch { .. } | ReviewTarget::PullRequest { .. }
    );
    let environment_error = if scope_requires_executor {
        sess.services
            .turn_environments
            .resolve_primary_environment()
            .await
            .err()
            .map(|err| anyhow::anyhow!("failed to start review scope environment: {err}"))
    } else {
        None
    };
    let turn_context = sess
        .new_turn_with_default_settings(sub_id.clone(), Default::default())
        .await;
    let resolved = if let Some(err) = environment_error {
        Err(err)
    } else if scope_requires_executor {
        if let Some(environment) = turn_context.initial_environments.primary() {
            let runner = ExecutorReviewCommandRunner::new(
                environment.environment.get_exec_backend(),
                &turn_context.config.permissions.shell_environment_policy,
            );
            crate::review_prompts::resolve_review_request_with_runner(
                review_request,
                &runner,
                environment.cwd(),
            )
            .await
        } else {
            Err(anyhow::anyhow!(
                "cannot resolve review scope without a selected environment"
            ))
        }
    } else {
        #[allow(deprecated)]
        crate::review_prompts::resolve_review_request(review_request, &turn_context.cwd).await
    };
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(err) => {
            sess.emit_turn_error_lifecycle(turn_context.as_ref(), CodexErrorInfo::Other)
                .await;
            sess.send_event(
                &turn_context,
                EventMsg::Error(ErrorEvent {
                    misalignment: None,
                    message: err.to_string(),
                    codex_error_info: Some(CodexErrorInfo::Other),
                }),
            )
            .await;
            let terminal_error = turn_context.terminal_error.lock().await.clone();
            sess.send_event(
                &turn_context,
                EventMsg::TurnComplete(TurnCompleteEvent {
                    turn_id: sub_id,
                    last_agent_message: None,
                    error: terminal_error,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                    time_to_first_token_ms: None,
                }),
            )
            .await;
            return;
        }
    };
    if let Some(metadata) = resolved.pull_request_context {
        turn_context
            .extension_data
            .insert(PullRequestContext::new(metadata));
    }
    let item = codex_protocol::items::TurnItem::EnteredReviewMode(
        codex_protocol::items::EnteredReviewModeItem {
            id: uuid::Uuid::now_v7().to_string(),
            target: resolved.target,
            user_facing_hint: resolved.user_facing_hint,
        },
    );
    sess.emit_turn_item_started(turn_context.as_ref(), &item)
        .await;
    sess.emit_turn_item_completed(turn_context.as_ref(), item)
        .await;
    sess.spawn_task(
        turn_context,
        vec![TurnInput::UserInput {
            content: vec![UserInput::Text {
                text: resolved.prompt,
                text_elements: Vec::new(),
            }],
            client_id: None,
            acceptance_order: None,
        }],
        ReviewTask::new(),
    )
    .await;
}
