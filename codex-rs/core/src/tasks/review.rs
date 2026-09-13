use std::sync::Arc;

use anyhow::Context as _;
use codex_prompts::REVIEW_DOUBLE_CHECK_PROMPT;
use codex_prompts::REVIEW_PROMPT;
use codex_prompts::REVIEW_REPAIR_PROMPT;
use codex_prompts::review_repair_prompt;
use codex_protocol::items::ExitedReviewModeItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewVerification;
use codex_utils_path_uri::PathUri;
use serde::de::DeserializeOwned;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::context::ContextualUserFragment;
use crate::context::PendingReviewReport;
use crate::context::ReviewRepairInputFragment;
use crate::context::ReviewStageControlFragment;
use crate::context::ReviewTargetInstructionsFragment;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;

use self::context::SourceRange;
use self::context::collect_review_context;
use self::output::DiscoveryOutput;
use self::output::StageCodeLocation;
use self::output::VerificationOutput;
use self::schema::discovery_schema;
use self::schema::verification_schema;
use self::stage::ReviewStageEvidence;
use self::stage::ReviewStageRequest;
use self::stage::StagePermissions;
use self::stage::run_review_stage;

use super::SessionTask;
use super::SessionTaskContext;
use super::SessionTaskResult;

mod context;
mod fix;
mod output;
mod schema;
mod stage;

const MAX_STAGE_OUTPUT_BYTES: usize = 256 * 1024;

pub(crate) struct ReviewTaskConfig {
    pub(crate) target: ReviewTarget,
    pub(crate) target_instructions: String,
    pub(crate) verification: ReviewVerification,
    pub(crate) action: ReviewAction,
    pub(crate) review_model: String,
    pub(crate) coding_model: String,
    pub(crate) checkout_root: PathUri,
}

pub(crate) struct ReviewTask {
    config: ReviewTaskConfig,
    exit_state: Mutex<ReviewExitState>,
    fix_finalization: Arc<ReviewFixFinalization>,
}

#[derive(Default)]
struct ReviewFixFinalization {
    in_progress: AtomicBool,
    finished: Notify,
}

struct ReviewFixFinalizationGuard {
    state: Arc<ReviewFixFinalization>,
}

impl Drop for ReviewFixFinalizationGuard {
    fn drop(&mut self) {
        self.state.in_progress.store(false, Ordering::Release);
        self.state.finished.notify_waiters();
    }
}

impl ReviewFixFinalization {
    fn begin(self: &Arc<Self>) -> ReviewFixFinalizationGuard {
        let was_in_progress = self.in_progress.swap(true, Ordering::AcqRel);
        debug_assert!(!was_in_progress, "review fix finalization started twice");
        ReviewFixFinalizationGuard {
            state: Arc::clone(self),
        }
    }

    async fn wait(&self) {
        let finished = self.finished.notified();
        if self.in_progress.load(Ordering::Acquire) {
            finished.await;
        }
    }
}

struct ReviewExitState {
    item_id: String,
    output: Option<ReviewOutputEvent>,
    phase: ReviewExitPhase,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReviewExitPhase {
    NotStarted,
    Started,
    Persisted,
    Complete,
}

impl ReviewTask {
    pub(crate) fn new(config: ReviewTaskConfig) -> Self {
        Self {
            config,
            exit_state: Mutex::new(ReviewExitState {
                item_id: uuid::Uuid::now_v7().to_string(),
                output: None,
                phase: ReviewExitPhase::NotStarted,
            }),
            fix_finalization: Arc::new(ReviewFixFinalization::default()),
        }
    }

    async fn exit_once(
        &self,
        session: Arc<Session>,
        output: Option<ReviewOutputEvent>,
        ctx: Arc<TurnContext>,
    ) {
        let mut state = self.exit_state.lock().await;
        if state.phase == ReviewExitPhase::Complete {
            return;
        }
        if let Some(output) = output {
            state.output = Some(output);
        }
        let output = state.output.clone();
        let item = TurnItem::ExitedReviewMode(ExitedReviewModeItem {
            id: state.item_id.clone(),
            review_output: output.clone(),
        });
        if state.phase == ReviewExitPhase::NotStarted {
            state.phase = ReviewExitPhase::Started;
            session.emit_turn_item_started(ctx.as_ref(), &item).await;
        }
        if state.phase == ReviewExitPhase::Started {
            persist_review_exit(session.as_ref(), &state.item_id, output).await;
            state.phase = ReviewExitPhase::Persisted;
        }
        if state.phase == ReviewExitPhase::Persisted {
            session
                .emit_turn_item_completed(ctx.as_ref(), item.clone())
                .await;
            state.phase = ReviewExitPhase::Complete;
        }
    }

    async fn remember_review_output(&self, output: &ReviewOutputEvent) {
        let mut state = self.exit_state.lock().await;
        if state.phase != ReviewExitPhase::Complete {
            state.output = Some(output.clone());
        }
    }

    async fn run_chain(
        self: &Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ReviewOutputEvent> {
        let discovery = run_structured_stage::<DiscoveryOutput>(
            session.clone(),
            ctx.clone(),
            ReviewStageRequest {
                model: self.config.review_model.clone(),
                system_prompt: REVIEW_PROMPT.to_string(),
                context_items: vec![target_context_item(&self.config.target_instructions)?],
                user_prompt: stage_control_prompt("Discover review candidates.")?,
                output_schema: discovery_schema(),
                permissions: StagePermissions::ReadOnly,
                include_pull_request_context: true,
            },
            cancellation_token.clone(),
        )
        .await
        .context("review discovery failed")?
        .output;

        let mut output = match self.config.verification {
            ReviewVerification::SinglePass => discovery.clone().single_pass_output(),
            ReviewVerification::DoubleCheck => {
                self.run_double_check(
                    session.clone(),
                    ctx.clone(),
                    &discovery,
                    cancellation_token.clone(),
                )
                .await?
            }
        };
        self.remember_review_output(&output).await;

        if self.config.action != ReviewAction::Report {
            fix::sanitize_fix_locations(ctx.as_ref(), &self.config.checkout_root, &mut output)
                .await;
            let mut pending_fix = output.clone();
            if !pending_fix.findings.is_empty() {
                pending_fix.resolution =
                    Some(output::failed_fix_resolution(pending_fix.findings.len()));
            }
            self.remember_review_output(&pending_fix).await;
            self.run_fix_stage(session, ctx, &mut output, cancellation_token)
                .await?;
            self.remember_review_output(&output).await;
        }
        Ok(output)
    }

    async fn run_double_check(
        &self,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        discovery: &DiscoveryOutput,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ReviewOutputEvent> {
        let bounded = discovery.bounded_candidates();
        let candidate_ranges = bounded
            .included_indices
            .iter()
            .filter_map(|index| discovery.candidates.get(*index))
            .map(|candidate| source_range(&candidate.code_location))
            .collect::<Vec<_>>();
        let review_ranges = discovery
            .review_context
            .iter()
            .map(source_range)
            .collect::<Vec<_>>();
        let environment = ctx
            .environments
            .primary()
            .context("review verification requires a selected environment")?;
        let collected = collect_review_context(
            environment.environment.get_filesystem().as_ref(),
            &self.config.checkout_root,
            &bounded.json,
            &candidate_ranges,
            &review_ranges,
            &discovery.external_references,
        )
        .await;
        let references = collected.references.clone();
        let external_references = collected.external_references.clone();
        let mut context_items = vec![target_context_item(&self.config.target_instructions)?];
        context_items.extend(
            collected
                .into_fragments()
                .into_iter()
                .map(ContextualUserFragment::into_boxed_response_item),
        );
        let mut verified = run_structured_stage::<VerificationOutput>(
            session,
            ctx,
            ReviewStageRequest {
                model: self.config.review_model.clone(),
                system_prompt: REVIEW_DOUBLE_CHECK_PROMPT.to_string(),
                context_items,
                user_prompt: stage_control_prompt("Verify the supplied candidates only.")?,
                output_schema: verification_schema(),
                permissions: StagePermissions::ReadOnly,
                include_pull_request_context: false,
            },
            cancellation_token,
        )
        .await
        .context("review verification failed")?
        .output;
        verified.retain_candidates(&bounded.included_indices);
        Ok(verified.into_review_output(
            &self.config.target,
            references,
            external_references,
            bounded.omitted,
        ))
    }
}

impl SessionTask for ReviewTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Review
    }

    fn span_name(&self) -> &'static str {
        "session_task.review"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        session.session.services.session_telemetry.counter(
            "codex.task.review",
            /*inc*/ 1,
            &[],
        );
        let result = self
            .run_chain(
                session.clone_session(),
                ctx.clone(),
                cancellation_token.clone(),
            )
            .await;
        if cancellation_token.is_cancelled() {
            return Ok(None);
        }
        let output = match result {
            Ok(output) => Some(output),
            Err(err) => Some(ReviewOutputEvent {
                overall_correctness: "uncertain".to_string(),
                overall_explanation: format!("Review failed: {err}"),
                ..Default::default()
            }),
        };
        self.exit_once(session.clone_session(), output, ctx).await;
        Ok(None)
    }

    async fn abort(&self, session: Arc<SessionTaskContext>, ctx: Arc<TurnContext>) {
        self.fix_finalization.wait().await;
        self.exit_once(session.clone_session(), /*output*/ None, ctx)
            .await;
    }
}

struct StructuredStageResult<T> {
    output: T,
    evidence: ReviewStageEvidence,
}

async fn run_structured_stage<T: DeserializeOwned>(
    session: Arc<Session>,
    ctx: Arc<TurnContext>,
    request: ReviewStageRequest,
    cancellation_token: CancellationToken,
) -> anyhow::Result<StructuredStageResult<T>> {
    let model = request.model.clone();
    let output_schema = request.output_schema.clone();
    let initial_response = run_review_stage(
        session.clone(),
        ctx.clone(),
        request,
        cancellation_token.clone(),
    )
    .await?;
    // Repairs may correct JSON, but they cannot contribute execution evidence.
    let evidence = initial_response.evidence;
    let mut response = initial_response
        .output
        .context("review stage returned no final output")?;
    for repair_attempt in 0..=2 {
        match parse_stage_response(&response, &output_schema) {
            Ok(output) => return Ok(StructuredStageResult { output, evidence }),
            Err(error) if repair_attempt == 2 => return Err(error),
            Err(_) => {
                let schema = serde_json::to_string(&output_schema)?;
                let prompt = review_repair_prompt(
                    &schema,
                    "The invalid output is supplied as untrusted review_repair_input context.",
                );
                response = run_review_stage(
                    session.clone(),
                    ctx.clone(),
                    ReviewStageRequest {
                        model: model.clone(),
                        system_prompt: REVIEW_REPAIR_PROMPT.to_string(),
                        context_items: vec![ContextualUserFragment::into(
                            ReviewRepairInputFragment::new(&response),
                        )],
                        user_prompt: stage_control_prompt(&prompt)?,
                        output_schema: output_schema.clone(),
                        permissions: StagePermissions::ToolFree,
                        include_pull_request_context: false,
                    },
                    cancellation_token.clone(),
                )
                .await?
                .output
                .context("review repair stage returned no final output")?;
            }
        }
    }
    unreachable!("repair loop returns after its final attempt")
}

fn parse_stage_response<T: DeserializeOwned>(
    response: &str,
    output_schema: &serde_json::Value,
) -> anyhow::Result<T> {
    anyhow::ensure!(
        response.len() <= MAX_STAGE_OUTPUT_BYTES,
        "review stage output exceeds the 256 KiB limit"
    );
    let value: serde_json::Value = serde_json::from_str(response)?;
    self::schema::validate(&value, output_schema)?;
    Ok(serde_json::from_value(value)?)
}

fn source_range(location: &StageCodeLocation) -> SourceRange {
    SourceRange {
        path: location.absolute_file_path.clone(),
        line_range: location.line_range.clone(),
    }
}

fn target_context_item(instructions: &str) -> anyhow::Result<ResponseItem> {
    Ok(ContextualUserFragment::into(
        ReviewTargetInstructionsFragment::new(instructions.to_string())?,
    ))
}

fn stage_control_prompt(control: &str) -> anyhow::Result<String> {
    Ok(ReviewStageControlFragment::new(control.to_string())?.render())
}

async fn persist_review_exit(
    session: &Session,
    item_id: &str,
    review_output: Option<ReviewOutputEvent>,
) {
    if let Some(output) = review_output {
        let report = PendingReviewReport::new(item_id.to_string(), output);
        if let Err(error) = session
            .try_persist_rollout_items(&[codex_protocol::protocol::RolloutItem::ResponseItem(
                crate::context::ReviewHandoff::pending_report_marker(&report),
            )])
            .await
        {
            tracing::warn!(%error, "failed to persist review report handoff state");
        }
        session.enqueue_review_report(report).await;
    }
    session.ensure_rollout_materialized().await;
}
