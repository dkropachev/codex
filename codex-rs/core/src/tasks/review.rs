use std::sync::Arc;

use anyhow::Context as _;
use codex_git_utils::validate_review_fix_commit_target;
use codex_prompts::REVIEW_DOUBLE_CHECK_PROMPT;
use codex_prompts::REVIEW_PROMPT;
use codex_prompts::REVIEW_REPAIR_PROMPT;
use codex_prompts::review_repair_prompt;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::models::ResponseItem;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::protocol::ReviewAction;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
use codex_protocol::protocol::ReviewVerification;
use codex_utils_path_uri::PathUri;
use serde::de::DeserializeOwned;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::context::ContextualUserFragment;
use crate::context::ReviewRepairInputFragment;
use crate::context::ReviewStageControlFragment;
use crate::context::ReviewTargetInstructionsFragment;
use crate::session::ExecutorReviewCommandRunner;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;

use self::context::SourceRange;
use self::context::collect_review_context_with_sandbox;
use self::output::DiscoveryOutput;
use self::output::StageCodeLocation;
use self::output::VerificationOutput;
use self::output::normalize_review_assessment;
use self::schema::discovery_schema;
use self::schema::verification_schema;
use self::stage::ReviewStageEvidence;
use self::stage::ReviewStageRequest;
use self::stage::StagePermissions;
use self::stage::review_fix_permission_profile;
use self::stage::run_review_stage;

use super::SessionTask;
use super::SessionTaskContext;
use super::SessionTaskResult;

mod context;
mod exit;
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
    exit_state: Arc<exit::ReviewExitCoordinator>,
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
        loop {
            let finished = self.finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if !self.in_progress.load(Ordering::Acquire) {
                return;
            }
            finished.await;
        }
    }
}

impl ReviewTask {
    pub(crate) fn new(config: ReviewTaskConfig) -> Self {
        Self {
            config,
            exit_state: Arc::new(exit::ReviewExitCoordinator::new()),
            fix_finalization: Arc::new(ReviewFixFinalization::default()),
        }
    }

    async fn exit_once(
        &self,
        session: Arc<Session>,
        output: Option<ReviewOutputEvent>,
        ctx: Arc<TurnContext>,
    ) {
        self.exit_state.exit_once(session, output, ctx).await;
    }

    async fn remember_review_output(&self, output: &ReviewOutputEvent) {
        self.exit_state.remember_review_output(output).await;
    }

    async fn run_chain(
        self: &Arc<Self>,
        session: Arc<Session>,
        ctx: Arc<TurnContext>,
        cancellation_token: CancellationToken,
    ) -> anyhow::Result<ReviewOutputEvent> {
        let environment = ctx
            .environments
            .primary()
            .context("review requires a selected environment")?;
        let parent_sandbox = ctx.file_system_sandbox_context(
            /*additional_permissions*/ None,
            &self.config.checkout_root,
        );
        environment
            .environment
            .get_filesystem()
            .canonicalize(&self.config.checkout_root, Some(&parent_sandbox))
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Review cannot read the checkout under the active permissions: {error}"
                )
            })?;
        let read_deny_entries = match &parent_sandbox.permissions {
            PermissionProfile::Managed {
                file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
                ..
            } => entries
                .iter()
                .filter(|entry| entry.access == FileSystemAccessMode::Deny)
                .cloned()
                .collect::<Vec<_>>(),
            PermissionProfile::Managed {
                file_system: ManagedFileSystemPermissions::Unrestricted,
                ..
            }
            | PermissionProfile::Disabled
            | PermissionProfile::External { .. } => Vec::new(),
        };
        if !read_deny_entries.is_empty() {
            ctx.extension_data
                .insert(crate::review_stage_runtime::ReviewReadDenyEntries(
                    read_deny_entries,
                ));
        }
        if cfg!(target_os = "windows")
            && ctx
                .environments
                .primary()
                .is_some_and(|environment| !environment.environment.is_remote())
            && ctx.windows_sandbox_level
                == codex_protocol::config_types::WindowsSandboxLevel::Disabled
        {
            anyhow::bail!(
                "Review requires the Windows sandbox for a local environment; enable unelevated or elevated Windows sandboxing"
            );
        }
        if self.config.action != ReviewAction::Report {
            let fix_permissions = review_fix_permission_profile();
            ctx.config
                .permissions
                .can_set_permission_profile(&fix_permissions)
                .context("Fix is unavailable under the active permission constraints")?;
            anyhow::ensure!(
                ctx.config.is_permission_profile_allowed(
                    codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
                    &fix_permissions,
                ),
                "Fix is disabled by the active permission requirements"
            );
        }
        if self.config.action == ReviewAction::FixAndCommit {
            let runner = ExecutorReviewCommandRunner::new(
                environment.environment.get_exec_backend(),
                &ctx.config.permissions.shell_environment_policy,
            );
            validate_review_fix_commit_target(&runner, &self.config.checkout_root).await?;
        }
        let mut read_only_paths = if self.config.action == ReviewAction::Report {
            Vec::new()
        } else {
            parent_read_only_checkout_paths(
                &parent_sandbox.permissions,
                &self.config.checkout_root,
            )?
        };
        match self
            .resolve_git_protected_paths(ctx.as_ref(), &parent_sandbox)
            .await
        {
            Ok(paths) => read_only_paths.extend(paths),
            Err(error) if self.config.action != ReviewAction::FixAndCommit => {
                tracing::debug!(%error, "review Git metadata paths were unavailable");
            }
            Err(error) => {
                return Err(error).context("Fix requires an accessible Git repository");
            }
        }
        if !read_only_paths.is_empty() {
            ctx.extension_data
                .insert(crate::review_stage_runtime::ReviewProtectedPaths(
                    read_only_paths,
                ));
        }
        if ctx
            .environments
            .primary()
            .is_some_and(|environment| !environment.environment.is_remote())
            && let Some(executable_path) = std::env::current_exe()
                .ok()
                .and_then(|path| PathUri::from_host_native_path(path).ok())
            && environment
                .environment
                .get_filesystem()
                .canonicalize(&executable_path, Some(&parent_sandbox))
                .await
                .is_ok()
        {
            ctx.extension_data
                .insert(crate::review_stage_runtime::ReviewAdditionalReadPaths(
                    vec![executable_path],
                ));
        }
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
                workspace_read_root: Some(self.config.checkout_root.clone()),
                workspace_write_root: None,
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
        if discovery.candidates.is_empty() {
            return Ok(discovery.clone().single_pass_output());
        }
        let mut bounded = discovery.bounded_candidates();
        if bounded.included_indices.is_empty() {
            let mut output = discovery.clone().single_pass_output();
            output.unverified_findings.append(&mut output.findings);
            normalize_review_assessment(&self.config.target, &mut output);
            return Ok(output);
        }
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
        let collector_sandbox = ctx.file_system_sandbox_context(
            /*additional_permissions*/ None,
            &self.config.checkout_root,
        );
        let collected = collect_review_context_with_sandbox(
            environment.environment.get_filesystem().as_ref(),
            &self.config.checkout_root,
            &collector_sandbox,
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
                workspace_read_root: Some(self.config.checkout_root.clone()),
                workspace_write_root: None,
                include_pull_request_context: matches!(
                    self.config.target,
                    ReviewTarget::PullRequest { .. }
                ),
            },
            cancellation_token,
        )
        .await
        .context("review verification failed")?
        .output;
        let missing = verified.retain_candidates(&bounded.included_indices);
        bounded
            .omitted
            .extend(missing.into_iter().filter_map(|index| {
                discovery
                    .candidates
                    .get(index)
                    .cloned()
                    .map(output::StageFinding::into_review_finding)
            }));
        Ok(verified.into_review_output(
            &self.config.target,
            &discovery.candidates,
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
                let prompt = review_repair_prompt();
                response = run_review_stage(
                    session.clone(),
                    ctx.clone(),
                    ReviewStageRequest {
                        model: model.clone(),
                        system_prompt: REVIEW_REPAIR_PROMPT.to_string(),
                        context_items: vec![ContextualUserFragment::into(
                            ReviewRepairInputFragment::new(&response),
                        )],
                        user_prompt: stage_control_prompt(prompt)?,
                        output_schema: output_schema.clone(),
                        permissions: StagePermissions::ToolFree,
                        workspace_read_root: None,
                        workspace_write_root: None,
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

fn parent_read_only_checkout_paths(
    permissions: &PermissionProfile<PathUri>,
    checkout_root: &PathUri,
) -> anyhow::Result<Vec<PathUri>> {
    let PermissionProfile::Managed {
        file_system: ManagedFileSystemPermissions::Restricted { entries, .. },
        ..
    } = permissions
    else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.access == FileSystemAccessMode::Read)
    {
        match &entry.path {
            FileSystemPath::Path { path }
                if path != checkout_root && path.starts_with(checkout_root) =>
            {
                paths.push(path.clone());
            }
            FileSystemPath::Special {
                value:
                    FileSystemSpecialPath::ProjectRoots {
                        subpath: Some(path),
                    },
            } => paths.push(checkout_root.join(path.to_string_lossy().as_ref())?),
            FileSystemPath::GlobPattern { .. } => {
                anyhow::bail!("Fix cannot preserve a read-only glob inside the checkout");
            }
            FileSystemPath::Path { .. } | FileSystemPath::Special { .. } => {}
        }
    }
    Ok(paths)
}
