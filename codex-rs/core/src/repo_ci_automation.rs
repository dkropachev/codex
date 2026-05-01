use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_config::config_toml::RepoCiAutomationToml;
use codex_config::config_toml::RepoCiScopeToml;
use codex_protocol::config_types::WebSearchMode;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RepoCiIssueType;
use codex_protocol::protocol::RepoCiPhase;
use codex_protocol::protocol::RepoCiScope;
use codex_protocol::protocol::RepoCiSessionMode;
use codex_protocol::protocol::RepoCiState;
use codex_protocol::protocol::RepoCiStatusEvent;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::Prompt;
use crate::ResponseEvent;
use crate::codex_delegate::run_codex_thread_one_shot;
use crate::context::ContextualUserFragment;
use crate::context::RepoCiFollowup;
use crate::model_router::AvailableRouterModel;
use crate::model_router::ModelRouterSource;
use crate::model_router::apply_model_router;
use crate::model_router::auth_manager_for_config;
use crate::model_router::available_router_models;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

const MAX_OUTPUT_BYTES: usize = 24_000;
const MAX_CHANGED_PATHS_BYTES: usize = 8_000;
const MAX_FOLLOWUP_OUTPUT_BYTES: usize = 1_200;
const MAX_FOLLOWUP_CHANGED_PATHS_BYTES: usize = 600;
const MAX_FOLLOWUP_DIFF_BYTES: usize = 800;
const MAX_FOLLOWUP_TRIAGE_SUMMARY_BYTES: usize = 400;
const MAX_FOLLOWUP_REVIEW_SUMMARY_BYTES: usize = 500;
const MAX_FOLLOWUP_FINDINGS_BYTES: usize = 1_800;
const MAX_FIX_WORKER_FINDINGS: usize = 8;
const MAX_FIX_WORKER_FINDINGS_BYTES: usize = 6_000;
const MAX_FIX_WORKER_FINDING_TITLE_BYTES: usize = 300;
const MAX_FIX_WORKER_FINDING_BODY_BYTES: usize = 1_200;
const MAX_FIX_WORKER_FINDING_LOCATION_BYTES: usize = 500;
const TRIAGE_BASE_INSTRUCTIONS: &str =
    "You classify repository CI failures. Return strict JSON only. Do not suggest code edits.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoCiTurnState {
    initial_snapshot: WorktreeSnapshot,
    review_fix_rounds: u8,
    review_exhausted: bool,
    local_fix_rounds: u8,
    remote_fix_rounds: u8,
    remote_completed: bool,
}

impl RepoCiTurnState {
    pub(crate) fn new(cwd: &Path) -> Self {
        Self {
            initial_snapshot: WorktreeSnapshot::capture(cwd),
            review_fix_rounds: 0,
            review_exhausted: false,
            local_fix_rounds: 0,
            remote_fix_rounds: 0,
            remote_completed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeSnapshot {
    digest: String,
    changed_paths: Vec<String>,
    diff_summary: String,
}

impl WorktreeSnapshot {
    fn capture(cwd: &Path) -> Self {
        let mut hasher = Sha256::new();
        let mut changed_paths = Vec::new();
        for args in [
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"][..],
            &["diff", "--no-ext-diff", "--binary"][..],
            &["diff", "--cached", "--no-ext-diff", "--binary"][..],
        ] {
            if let Ok(output) = Command::new("git").args(args).current_dir(cwd).output() {
                hasher.update(args.join(" ").as_bytes());
                hasher.update(&output.stdout);
                hasher.update(&output.stderr);
                if args[0] == "status" {
                    changed_paths = parse_status_paths(&output.stdout);
                }
            }
        }
        let diff_summary = [&["diff", "--stat"][..], &["diff", "--stat", "--cached"][..]]
            .into_iter()
            .filter_map(|args| {
                Command::new("git")
                    .args(args)
                    .current_dir(cwd)
                    .output()
                    .ok()
                    .and_then(|output| output.status.success().then_some(output.stdout))
                    .map(|stdout| String::from_utf8_lossy(&stdout).to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            digest: format!("{:x}", hasher.finalize()),
            changed_paths,
            diff_summary,
        }
    }
}

fn parse_status_paths(stdout: &[u8]) -> Vec<String> {
    stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            if entry.len() < 4 {
                return None;
            }
            Some(String::from_utf8_lossy(&entry[3..]).to_string())
        })
        .collect()
}

fn repo_ci_owned_changed_paths(
    initial_snapshot: &WorktreeSnapshot,
    current_snapshot: &WorktreeSnapshot,
) -> Vec<String> {
    let initial_paths = initial_snapshot
        .changed_paths
        .iter()
        .collect::<BTreeSet<_>>();
    current_snapshot
        .changed_paths
        .iter()
        .filter(|path| !initial_paths.contains(path))
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveRepoCiConfig {
    automation: RepoCiAutomationToml,
    local_test_time_budget_sec: u64,
    long_ci: bool,
    max_local_fix_rounds: u8,
    max_remote_fix_rounds: u8,
    review_issue_types: Vec<RepoCiIssueType>,
    max_review_fix_rounds: u8,
}

impl EffectiveRepoCiConfig {
    fn from_scope(
        scope: &RepoCiScopeToml,
        review_issue_types: Vec<RepoCiIssueType>,
        long_ci: bool,
    ) -> Option<Self> {
        if scope.enabled != Some(true) {
            return None;
        }
        Some(Self {
            automation: scope
                .automation
                .unwrap_or(RepoCiAutomationToml::LocalAndRemote),
            local_test_time_budget_sec: scope.local_test_time_budget_sec.unwrap_or(300),
            long_ci,
            max_local_fix_rounds: scope.max_local_fix_rounds.unwrap_or(3),
            max_remote_fix_rounds: scope.max_remote_fix_rounds.unwrap_or(2),
            review_issue_types,
            max_review_fix_rounds: scope.max_review_fix_rounds.unwrap_or(2),
        })
    }

    fn from_session_mode(
        mode: RepoCiSessionMode,
        base: Option<&RepoCiScopeToml>,
        review_issue_types: Vec<RepoCiIssueType>,
        review_rounds: Option<u8>,
        long_ci: bool,
    ) -> Option<Self> {
        if mode == RepoCiSessionMode::Off {
            return None;
        }
        Some(Self {
            automation: session_mode_to_automation(mode),
            local_test_time_budget_sec: base
                .and_then(|scope| scope.local_test_time_budget_sec)
                .unwrap_or(300),
            long_ci,
            max_local_fix_rounds: base
                .and_then(|scope| scope.max_local_fix_rounds)
                .unwrap_or(3),
            max_remote_fix_rounds: base
                .and_then(|scope| scope.max_remote_fix_rounds)
                .unwrap_or(2),
            review_issue_types,
            max_review_fix_rounds: review_rounds
                .or_else(|| base.and_then(|scope| scope.max_review_fix_rounds))
                .unwrap_or(2),
        })
    }

    fn local_enabled(&self) -> bool {
        matches!(
            self.automation,
            RepoCiAutomationToml::Local | RepoCiAutomationToml::LocalAndRemote
        )
    }

    fn remote_enabled(&self) -> bool {
        matches!(
            self.automation,
            RepoCiAutomationToml::Remote | RepoCiAutomationToml::LocalAndRemote
        )
    }

    fn review_enabled(&self) -> bool {
        !self.review_issue_types.is_empty() && self.max_review_fix_rounds > 0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoCiReviewOutput {
    findings: Vec<RepoCiReviewFinding>,
    #[serde(default)]
    disregarded_findings: Vec<RepoCiDisregardedFinding>,
    summary: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RepoCiReviewFinding {
    title: String,
    body: String,
    issue_type: RepoCiIssueType,
    #[serde(default)]
    absolute_file_path: Option<PathBuf>,
    #[serde(default)]
    location_hint: Option<String>,
}

impl RepoCiReviewFinding {
    fn key(&self) -> String {
        review_item_key(self.issue_type, &self.title, &self.location())
    }

    fn location(&self) -> String {
        self.absolute_file_path
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| self.location_hint.clone())
            .unwrap_or_else(|| "repo".to_string())
    }

    fn summary_line(&self) -> String {
        format!(
            "- [{}] {} ({})",
            repo_ci_issue_type_slug(self.issue_type),
            self.title,
            self.location()
        )
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RepoCiDisregardedFinding {
    title: String,
    body: String,
    issue_type: RepoCiIssueType,
    #[serde(default)]
    absolute_file_path: Option<PathBuf>,
    #[serde(default)]
    location_hint: Option<String>,
    reason: String,
}

impl RepoCiDisregardedFinding {
    fn key(&self) -> String {
        review_item_key(self.issue_type, &self.title, &self.location())
    }

    fn location(&self) -> String {
        self.absolute_file_path
            .as_ref()
            .map(|path| path.display().to_string())
            .or_else(|| self.location_hint.clone())
            .unwrap_or_else(|| "repo".to_string())
    }

    fn summary_line(&self) -> String {
        format!(
            "- [{}] {} ({}) - {}: {}",
            repo_ci_issue_type_slug(self.issue_type),
            self.title,
            self.location(),
            self.reason,
            truncate_middle(&self.body, 240)
        )
    }
}

fn review_item_key(issue_type: RepoCiIssueType, title: &str, location: &str) -> String {
    format!(
        "{}|{}|{}",
        repo_ci_issue_type_slug(issue_type),
        title.trim(),
        location
    )
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoCiFixWorkerOutput {
    summary: String,
    touched_files: Vec<String>,
}

#[derive(Debug, Clone)]
struct RepoCiFixGroup {
    key: String,
    owned_paths: Vec<PathBuf>,
    findings: Vec<RepoCiReviewFinding>,
}

#[derive(Debug, Default)]
struct RepoCiReviewIssueTracker {
    active: BTreeMap<String, RepoCiReviewFinding>,
    resolved: BTreeMap<String, RepoCiReviewFinding>,
    disregarded: BTreeMap<String, RepoCiDisregardedFinding>,
}

impl RepoCiReviewIssueTracker {
    fn record_review(
        &mut self,
        review: &RepoCiReviewOutput,
        selected_issue_types: &[RepoCiIssueType],
    ) {
        let next_active = review
            .findings
            .iter()
            .map(|finding| (finding.key(), finding.clone()))
            .collect::<BTreeMap<_, _>>();
        for (key, finding) in &self.active {
            if !next_active.contains_key(key) {
                self.resolved
                    .entry(key.clone())
                    .or_insert_with(|| finding.clone());
            }
        }
        self.active = next_active;

        for finding in review
            .disregarded_findings
            .iter()
            .filter(|finding| !selected_issue_types.contains(&finding.issue_type))
        {
            self.disregarded
                .entry(finding.key())
                .or_insert_with(|| finding.clone());
        }
    }

    fn summary_message(&self) -> Option<String> {
        if self.active.is_empty() && self.resolved.is_empty() && self.disregarded.is_empty() {
            return None;
        }
        Some(format!(
            "Repo CI targeted review summary.\n\nUnresolved issues:\n{}\n\nResolved issues:\n{}\n\nDisregarded by issue-type filter:\n{}",
            format_review_item_lines(self.active.values().map(RepoCiReviewFinding::summary_line)),
            format_review_item_lines(
                self.resolved
                    .values()
                    .map(RepoCiReviewFinding::summary_line)
            ),
            format_review_item_lines(
                self.disregarded
                    .values()
                    .map(RepoCiDisregardedFinding::summary_line),
            )
        ))
    }
}

fn format_review_item_lines(lines: impl Iterator<Item = String>) -> String {
    let lines = lines.collect::<Vec<_>>();
    if lines.is_empty() {
        "- (none)".to_string()
    } else {
        lines.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RepoCiLearningRequirement {
    Initial,
    Stale(Vec<PathBuf>),
}

impl RepoCiLearningRequirement {
    fn started_message(&self) -> String {
        match self {
            Self::Initial => {
                "Repo CI learning started because this repository has not been learned yet."
                    .to_string()
            }
            Self::Stale(paths) => format!(
                "Repo CI relearning started because tracked learning sources changed: {}.",
                format_learning_paths(paths)
            ),
        }
    }

    fn passed_message(&self) -> String {
        match self {
            Self::Initial => {
                "Repo CI learning passed and validated a fresh local runner.".to_string()
            }
            Self::Stale(_) => {
                "Repo CI relearning passed and validated an updated local runner.".to_string()
            }
        }
    }

    fn failed_message(&self, err: &anyhow::Error) -> String {
        match self {
            Self::Initial => format!("Repo CI learning failed: {err:#}"),
            Self::Stale(_) => format!("Repo CI relearning failed: {err:#}"),
        }
    }
}

pub(crate) async fn maybe_run_after_agent(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    state: &mut RepoCiTurnState,
    cancellation_token: &CancellationToken,
) -> Option<ResponseItem> {
    let mut config = effective_config(turn_context)?;
    if !turn_context.config.active_project.is_trusted() {
        return None;
    }
    let mut current_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
    if current_snapshot == state.initial_snapshot {
        return None;
    }
    if ensure_repo_ci_learned(sess, turn_context, &config)
        .await
        .is_err()
    {
        return None;
    }
    if let Some(refreshed_config) = effective_config(turn_context) {
        config = refreshed_config;
    }

    let mut review_tracker = RepoCiReviewIssueTracker::default();
    let mut ran_review = false;
    let mut review_summary_state = RepoCiState::Passed;
    let mut review_summary_attempt = None;
    if config.review_enabled() && !state.review_exhausted {
        ran_review = true;
        loop {
            let review_attempt = state.review_fix_rounds.saturating_add(1);
            let review_snapshot = codex_repo_ci::BranchDiffSnapshot::capture(&turn_context.cwd);
            let review =
                match run_targeted_review(sess, turn_context, &config, &review_snapshot).await {
                    Ok(review) => review,
                    Err(err) => {
                        send_status(
                            sess,
                            turn_context,
                            RepoCiPhase::Triage,
                            RepoCiState::Failed,
                            RepoCiScope::Local,
                            Some(review_attempt),
                            Some(config.max_review_fix_rounds),
                            format!("Repo CI targeted review failed: {err:#}"),
                        )
                        .await;
                        review_summary_state = RepoCiState::Failed;
                        review_summary_attempt = Some(review_attempt);
                        break;
                    }
                };
            review_tracker.record_review(&review, &config.review_issue_types);
            if review.findings.is_empty() {
                state.review_fix_rounds = 0;
                state.review_exhausted = false;
                review_summary_attempt = Some(review_attempt);
                break;
            }
            if state.review_fix_rounds >= config.max_review_fix_rounds {
                state.review_exhausted = true;
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Triage,
                    RepoCiState::Exhausted,
                    RepoCiScope::Local,
                    Some(state.review_fix_rounds),
                    Some(config.max_review_fix_rounds),
                    "Repo CI targeted review still found issues after the configured review rounds."
                        .to_string(),
                )
                .await;
                review_summary_state = RepoCiState::Exhausted;
                review_summary_attempt = Some(state.review_fix_rounds);
                send_review_resolution_summary(
                    sess,
                    turn_context,
                    &review_tracker,
                    RepoCiState::Exhausted,
                    Some(state.review_fix_rounds),
                    Some(config.max_review_fix_rounds),
                )
                .await;
                break;
            }
            state.review_fix_rounds = state.review_fix_rounds.saturating_add(1);
            let fix_attempt = state.review_fix_rounds;
            let worker_outputs = match run_review_fix_workers(sess, turn_context, &review).await {
                Ok(worker_outputs) => worker_outputs,
                Err(err) => {
                    send_status(
                        sess,
                        turn_context,
                        RepoCiPhase::Triage,
                        RepoCiState::Failed,
                        RepoCiScope::Local,
                        Some(fix_attempt),
                        Some(config.max_review_fix_rounds),
                        format!("Repo CI fix workers failed: {err:#}"),
                    )
                    .await;
                    send_review_resolution_summary(
                        sess,
                        turn_context,
                        &review_tracker,
                        RepoCiState::Failed,
                        Some(fix_attempt),
                        Some(config.max_review_fix_rounds),
                    )
                    .await;
                    return Some(review_findings_prompt(&review));
                }
            };
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Triage,
                RepoCiState::Retrying,
                RepoCiScope::Local,
                Some(fix_attempt),
                Some(config.max_review_fix_rounds),
                aggregate_worker_summary(&worker_outputs),
            )
            .await;
            current_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
        }
    }

    macro_rules! send_final_review_summary {
        () => {
            if ran_review {
                send_review_resolution_summary(
                    sess,
                    turn_context,
                    &review_tracker,
                    review_summary_state.clone(),
                    review_summary_attempt,
                    Some(config.max_review_fix_rounds),
                )
                .await;
            }
        };
    }

    send_status(
        sess,
        turn_context,
        RepoCiPhase::Local,
        RepoCiState::Started,
        RepoCiScope::Local,
        None,
        None,
        "Repo CI local checks started.".to_string(),
    )
    .await;
    let repo_ci_cancellation = codex_repo_ci::RepoCiCancellation::default();
    let cancellation_task = tokio::spawn({
        let cancellation_token = cancellation_token.clone();
        let repo_ci_cancellation = repo_ci_cancellation.clone();
        async move {
            cancellation_token.cancelled().await;
            repo_ci_cancellation.cancel();
        }
    });
    let result = tokio::task::spawn_blocking({
        let codex_home = turn_context.config.codex_home.clone();
        let cwd = turn_context.cwd.clone();
        let config = config.clone();
        let repo_ci_cancellation = repo_ci_cancellation.clone();
        move || run_local_repo_ci(&codex_home, &cwd, &config, repo_ci_cancellation)
    })
    .await;
    cancellation_task.abort();
    let local_outcome = match result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(err)) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Local,
                RepoCiState::Failed,
                RepoCiScope::Local,
                None,
                None,
                format!("Repo CI failed to start: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
        Err(err) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Local,
                RepoCiState::Failed,
                RepoCiScope::Local,
                None,
                None,
                format!("Repo CI task failed: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
    };

    match local_outcome {
        LocalRepoCiOutcome::Skipped => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Local,
                RepoCiState::Skipped,
                RepoCiScope::Local,
                None,
                None,
                "Repo CI local checks skipped.".to_string(),
            )
            .await;
        }
        LocalRepoCiOutcome::Passed => {
            state.local_fix_rounds = 0;
            if !config.remote_enabled() || state.remote_completed {
                state.initial_snapshot = current_snapshot.clone();
            }
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Local,
                RepoCiState::Passed,
                RepoCiScope::Local,
                None,
                None,
                "Repo CI local checks passed.".to_string(),
            )
            .await;
        }
        LocalRepoCiOutcome::Failed { output } => {
            let triage = triage_failure(
                sess,
                turn_context,
                TriageInput {
                    kind: "local",
                    output: &output,
                    changed_paths: &current_snapshot.changed_paths,
                    diff_summary: &current_snapshot.diff_summary,
                    deterministic_classification: FailureClassification::Unknown,
                },
            )
            .await;
            if triage.should_ignore() {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Triage,
                    RepoCiState::Ignored,
                    RepoCiScope::Local,
                    None,
                    None,
                    format!(
                        "Repo CI local failure ignored as unrelated: {}",
                        triage.summary
                    ),
                )
                .await;
                state.initial_snapshot = current_snapshot.clone();
                send_final_review_summary!();
                return None;
            }
            if state.local_fix_rounds >= config.max_local_fix_rounds {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Local,
                    RepoCiState::Exhausted,
                    RepoCiScope::Local,
                    Some(state.local_fix_rounds),
                    Some(config.max_local_fix_rounds),
                    format!(
                        "Repo CI local checks are still failing after {} repair attempts.",
                        state.local_fix_rounds
                    ),
                )
                .await;
                send_final_review_summary!();
                return None;
            }
            state.local_fix_rounds = state.local_fix_rounds.saturating_add(1);
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Local,
                RepoCiState::Retrying,
                RepoCiScope::Local,
                Some(state.local_fix_rounds),
                Some(config.max_local_fix_rounds),
                format!(
                    "Repo CI local checks failed; starting repair attempt {} of {}.",
                    state.local_fix_rounds, config.max_local_fix_rounds
                ),
            )
            .await;
            send_final_review_summary!();
            return Some(repair_prompt(
                "local",
                &output,
                &current_snapshot.changed_paths,
                &current_snapshot.diff_summary,
                &triage,
            ));
        }
    }

    if !config.remote_enabled() || state.remote_completed {
        send_final_review_summary!();
        return None;
    }

    let workflow_start = tokio::task::spawn_blocking({
        let cwd = turn_context.cwd.clone();
        move || codex_repo_ci::start_remote_workflow(&cwd)
    })
    .await;
    let workflow = match workflow_start {
        Ok(Ok(codex_repo_ci::RemoteRepoCiWorkflowStart::Ready(workflow))) => workflow,
        Ok(Ok(codex_repo_ci::RemoteRepoCiWorkflowStart::Skipped(reason))) => {
            state.remote_completed = true;
            state.initial_snapshot = current_snapshot.clone();
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Skipped,
                RepoCiScope::Remote,
                None,
                None,
                reason,
            )
            .await;
            send_final_review_summary!();
            return None;
        }
        Ok(Err(err)) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Failed,
                RepoCiScope::Remote,
                None,
                None,
                format!("Repo CI remote checks failed before push: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
        Err(err) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Failed,
                RepoCiScope::Remote,
                None,
                None,
                format!("Repo CI remote preflight task failed: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
    };

    let remote_commit_paths =
        repo_ci_owned_changed_paths(&state.initial_snapshot, &current_snapshot);
    let remote_commit_decision =
        match prepare_remote_repo_ci_commit(sess, turn_context, &remote_commit_paths).await {
            Ok(decision) => decision,
            Err(err) => {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Remote,
                    RepoCiState::Failed,
                    RepoCiScope::Remote,
                    None,
                    None,
                    format!("Repo CI remote commit preparation failed: {err:#}"),
                )
                .await;
                send_final_review_summary!();
                return None;
            }
        };

    send_status(
        sess,
        turn_context,
        RepoCiPhase::Remote,
        RepoCiState::Started,
        RepoCiScope::Remote,
        None,
        None,
        "Repo CI remote checks started.".to_string(),
    )
    .await;
    let result = tokio::task::spawn_blocking({
        let cwd = turn_context.cwd.clone();
        let remote_commit_paths = remote_commit_paths.clone();
        move || {
            run_remote_repo_ci(
                &cwd,
                &workflow,
                remote_commit_decision.as_ref(),
                &remote_commit_paths,
            )
        }
    })
    .await;
    let remote_outcome = match result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(err)) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Failed,
                RepoCiScope::Remote,
                None,
                None,
                format!("Repo CI remote checks failed to run: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
        Err(err) => {
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Failed,
                RepoCiScope::Remote,
                None,
                None,
                format!("Repo CI remote task failed: {err:#}"),
            )
            .await;
            send_final_review_summary!();
            return None;
        }
    };

    match remote_outcome {
        RemoteRepoCiOutcome::Skipped(reason) => {
            state.remote_completed = true;
            state.initial_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Skipped,
                RepoCiScope::Remote,
                None,
                None,
                reason,
            )
            .await;
            send_final_review_summary!();
            None
        }
        RemoteRepoCiOutcome::Passed { prepared_commit } => {
            if let Some(applied) = prepared_commit {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Remote,
                    RepoCiState::Passed,
                    RepoCiScope::Remote,
                    None,
                    None,
                    format_remote_commit_applied(&applied),
                )
                .await;
            }
            state.remote_completed = true;
            state.initial_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Passed,
                RepoCiScope::Remote,
                None,
                None,
                "Repo CI remote checks passed.".to_string(),
            )
            .await;
            send_final_review_summary!();
            None
        }
        RemoteRepoCiOutcome::Failed {
            output,
            classification,
        } => {
            let triage = triage_failure(
                sess,
                turn_context,
                TriageInput {
                    kind: "remote",
                    output: &output,
                    changed_paths: &current_snapshot.changed_paths,
                    diff_summary: &current_snapshot.diff_summary,
                    deterministic_classification: classification,
                },
            )
            .await;
            if triage.should_ignore() {
                state.remote_completed = true;
                state.initial_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Triage,
                    RepoCiState::Ignored,
                    RepoCiScope::Remote,
                    None,
                    None,
                    format!(
                        "Repo CI remote failure ignored as unrelated: {}",
                        triage.summary
                    ),
                )
                .await;
                send_final_review_summary!();
                return None;
            }
            if state.remote_fix_rounds >= config.max_remote_fix_rounds {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Remote,
                    RepoCiState::Exhausted,
                    RepoCiScope::Remote,
                    Some(state.remote_fix_rounds),
                    Some(config.max_remote_fix_rounds),
                    format!(
                        "Repo CI remote checks are still failing after {} repair attempts.",
                        state.remote_fix_rounds
                    ),
                )
                .await;
                send_final_review_summary!();
                return None;
            }
            state.remote_fix_rounds = state.remote_fix_rounds.saturating_add(1);
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Retrying,
                RepoCiScope::Remote,
                Some(state.remote_fix_rounds),
                Some(config.max_remote_fix_rounds),
                format!(
                    "Repo CI remote checks failed; starting repair attempt {} of {}.",
                    state.remote_fix_rounds, config.max_remote_fix_rounds
                ),
            )
            .await;
            send_final_review_summary!();
            Some(repair_prompt(
                "remote",
                &output,
                &current_snapshot.changed_paths,
                &current_snapshot.diff_summary,
                &triage,
            ))
        }
    }
}

async fn ensure_repo_ci_learned(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    config: &EffectiveRepoCiConfig,
) -> Result<()> {
    let status = codex_repo_ci::status(&turn_context.config.codex_home, &turn_context.cwd)?;
    let Some(requirement) = repo_ci_learning_requirement(&status) else {
        return Ok(());
    };
    let result = async {
        let repo_root = status.paths.repo_root;
        let learning_hints = codex_repo_ci::collect_learning_hints(&repo_root)?;
        let mut prior_plan = None;
        let mut failure_feedback = None;

        send_status(
            sess,
            turn_context,
            RepoCiPhase::Learning,
            RepoCiState::Started,
            RepoCiScope::None,
            None,
            None,
            requirement.started_message(),
        )
        .await;

        for attempt in 1..=codex_repo_ci::AI_LEARN_MAX_ATTEMPTS {
            let prompt = codex_repo_ci::render_repo_ci_learning_prompt(
                &repo_root,
                &learning_hints,
                config.local_test_time_budget_sec,
                attempt,
                prior_plan.as_ref(),
                failure_feedback.as_deref(),
            );
            let plan: codex_repo_ci::RepoCiAiLearnedPlan = run_repo_ci_learning_subagent_json(
                sess,
                turn_context,
                prompt,
                codex_repo_ci::repo_ci_ai_plan_schema(),
            )
            .await?;
            let outcome = codex_repo_ci::learn_with_plan(
                &turn_context.config.codex_home,
                &repo_root,
                codex_repo_ci::LearnOptions {
                    automation: automation_to_repo_ci(config.automation),
                    local_test_time_budget_sec: config.local_test_time_budget_sec,
                },
                plan.clone().into_learned_plan()?,
            )?;
            if matches!(
                outcome.manifest.validation,
                codex_repo_ci::ValidationStatus::Passed { .. }
            ) {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Learning,
                    RepoCiState::Passed,
                    RepoCiScope::None,
                    Some(attempt as u8),
                    Some(codex_repo_ci::AI_LEARN_MAX_ATTEMPTS as u8),
                    requirement.passed_message(),
                )
                .await;
                return Ok(());
            }

            failure_feedback = Some(codex_repo_ci::render_validation_feedback(&outcome)?);
            prior_plan = Some(plan);
            if attempt < codex_repo_ci::AI_LEARN_MAX_ATTEMPTS {
                send_status(
                    sess,
                    turn_context,
                    RepoCiPhase::Learning,
                    RepoCiState::Retrying,
                    RepoCiScope::None,
                    Some(attempt as u8),
                    Some(codex_repo_ci::AI_LEARN_MAX_ATTEMPTS as u8),
                    format!(
                        "Repo CI learning validation failed on attempt {attempt}; retrying with a repaired plan."
                    ),
                )
                .await;
            }
        }

        anyhow::bail!(
            "repo-ci learner could not produce a passing runner after {} attempts",
            codex_repo_ci::AI_LEARN_MAX_ATTEMPTS
        )
    }
    .await;
    if let Err(err) = &result {
        send_status(
            sess,
            turn_context,
            RepoCiPhase::Learning,
            RepoCiState::Failed,
            RepoCiScope::None,
            None,
            None,
            requirement.failed_message(err),
        )
        .await;
    }
    result
}

async fn prepare_remote_repo_ci_commit(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    owned_paths: &[String],
) -> Result<Option<codex_repo_ci::RemoteCommitDecision>> {
    let context = tokio::task::spawn_blocking({
        let cwd = turn_context.cwd.clone();
        let owned_paths = owned_paths.to_vec();
        move || codex_repo_ci::remote_commit_decision_context(&cwd, &owned_paths)
    })
    .await
    .context("repo-ci remote commit inspection task failed")??;
    let Some(context) = context else {
        return Ok(None);
    };

    send_status(
        sess,
        turn_context,
        RepoCiPhase::Remote,
        RepoCiState::Started,
        RepoCiScope::Remote,
        None,
        None,
        format!(
            "Repo CI remote commit preparation started for {} changed path(s).",
            context.changed_paths.len()
        ),
    )
    .await;

    let prompt = codex_repo_ci::render_remote_commit_decision_prompt(&context);
    let decision = match run_repo_ci_read_only_subagent_json::<codex_repo_ci::RemoteCommitDecision>(
        sess,
        turn_context,
        ModelRouterSource::Module("repo_ci.commit"),
        SubAgentSource::Other("repo_ci_commit".to_string()),
        prompt,
        codex_repo_ci::remote_commit_decision_schema(),
    )
    .await
    {
        Ok(decision) => decision,
        Err(err) => {
            warn!("repo-ci commit decision agent failed; using separate commit fallback: {err:#}");
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Remote,
                RepoCiState::Retrying,
                RepoCiScope::Remote,
                None,
                None,
                format!("Repo CI commit decision failed; using separate commit fallback: {err:#}"),
            )
            .await;
            codex_repo_ci::fallback_remote_commit_decision()
        }
    };

    Ok(Some(decision))
}

fn format_remote_commit_applied(applied: &codex_repo_ci::RemoteCommitApplied) -> String {
    match applied.strategy {
        codex_repo_ci::RemoteCommitStrategy::AmendPriorCommit => {
            "Repo CI amended the prior commit before remote checks.".to_string()
        }
        codex_repo_ci::RemoteCommitStrategy::SeparateCommit => format!(
            "Repo CI created commit `{}` before remote checks.",
            applied
                .title
                .as_deref()
                .unwrap_or("repo-ci: prepare remote retry")
        ),
    }
}

fn repo_ci_learning_requirement(
    status: &codex_repo_ci::StatusOutcome,
) -> Option<RepoCiLearningRequirement> {
    if status.manifest.is_none() {
        return Some(RepoCiLearningRequirement::Initial);
    }
    if status.stale_sources.is_empty() {
        None
    } else {
        Some(RepoCiLearningRequirement::Stale(
            status
                .stale_sources
                .iter()
                .map(|source| source.path.clone())
                .collect(),
        ))
    }
}

fn format_learning_paths(paths: &[PathBuf]) -> String {
    let formatted = paths
        .iter()
        .take(3)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    if paths.len() > formatted.len() {
        format!(
            "{} and {} more",
            formatted.join(", "),
            paths.len() - formatted.len()
        )
    } else {
        formatted.join(", ")
    }
}

fn inferred_issue_types(turn_context: &TurnContext) -> Option<Vec<RepoCiIssueType>> {
    codex_repo_ci::status(&turn_context.config.codex_home, &turn_context.cwd)
        .ok()?
        .manifest
        .and_then(|manifest| {
            (!manifest.inferred_issue_types.is_empty()).then_some(manifest.inferred_issue_types)
        })
}

async fn run_targeted_review(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    config: &EffectiveRepoCiConfig,
    snapshot: &codex_repo_ci::BranchDiffSnapshot,
) -> Result<RepoCiReviewOutput> {
    send_status(
        sess,
        turn_context,
        RepoCiPhase::Triage,
        RepoCiState::Started,
        RepoCiScope::Local,
        None,
        None,
        format!(
            "Repo CI targeted review started for {}.",
            config
                .review_issue_types
                .iter()
                .copied()
                .map(repo_ci_issue_type_slug)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .await;
    let prompt = targeted_review_prompt(config, snapshot);
    let output: RepoCiReviewOutput = run_repo_ci_subagent_json(
        sess,
        turn_context,
        ModelRouterSource::Module("repo_ci.review"),
        SubAgentSource::Other("repo_ci_review".to_string()),
        prompt,
        review_output_schema(),
    )
    .await?;
    send_status(
        sess,
        turn_context,
        RepoCiPhase::Triage,
        if output.findings.is_empty() {
            RepoCiState::Passed
        } else {
            RepoCiState::Failed
        },
        RepoCiScope::Local,
        None,
        None,
        if output.findings.is_empty() {
            "Repo CI targeted review found no scoped issues.".to_string()
        } else {
            format!(
                "Repo CI targeted review found {} issue group(s).",
                group_review_findings(&output.findings).len()
            )
        },
    )
    .await;
    Ok(output)
}

async fn run_review_fix_workers(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    review: &RepoCiReviewOutput,
) -> Result<Vec<RepoCiFixWorkerOutput>> {
    let groups = group_review_findings(&review.findings);
    let tasks = groups
        .into_iter()
        .enumerate()
        .map(|(index, group)| async move {
            let prompt = review_fix_prompt(&group);
            run_repo_ci_subagent_json::<RepoCiFixWorkerOutput>(
                sess,
                turn_context,
                ModelRouterSource::Module("repo_ci.fix"),
                SubAgentSource::Other(format!("repo_ci_fix_{index}")),
                prompt,
                review_fix_output_schema(),
            )
            .await
        });
    let results = join_all(tasks).await;
    let mut outputs = Vec::new();
    for result in results {
        outputs.push(result?);
    }
    Ok(outputs)
}

fn group_review_findings(findings: &[RepoCiReviewFinding]) -> Vec<RepoCiFixGroup> {
    let mut groups = HashMap::<String, RepoCiFixGroup>::new();
    for finding in findings {
        let key = finding.location();
        let entry = groups.entry(key.clone()).or_insert_with(|| RepoCiFixGroup {
            key: key.clone(),
            owned_paths: Vec::new(),
            findings: Vec::new(),
        });
        if let Some(path) = finding.absolute_file_path.clone()
            && !entry.owned_paths.contains(&path)
        {
            entry.owned_paths.push(path);
        }
        entry.findings.push(finding.clone());
    }
    let mut values = groups.into_values().collect::<Vec<_>>();
    values.sort_by(|left, right| left.key.cmp(&right.key));
    values
}

async fn run_repo_ci_subagent_json<T>(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_router_source: ModelRouterSource,
    subagent_source: SubAgentSource,
    prompt: String,
    schema: serde_json::Value,
) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let mut sub_agent_config =
        repo_ci_phase_config(sess, turn_context, model_router_source, prompt.len());
    if let Err(err) = sub_agent_config
        .web_search_mode
        .set(WebSearchMode::Disabled)
    {
        warn!("failed to disable web search for repo-ci subagent: {err}");
    }
    sub_agent_config.permissions.approval_policy =
        crate::config::Constrained::allow_only(codex_protocol::protocol::AskForApproval::Never);
    let codex = run_codex_thread_one_shot(
        sub_agent_config,
        Arc::clone(&sess.services.auth_manager),
        Arc::clone(&sess.services.models_manager),
        vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }],
        Arc::clone(sess),
        Arc::clone(turn_context),
        CancellationToken::new(),
        subagent_source,
        Some(schema),
        None,
    )
    .await?;
    let mut final_text = None;
    while let Ok(event) = codex.next_event().await {
        if let EventMsg::TurnComplete(turn_complete) = event.msg {
            final_text = turn_complete.last_agent_message;
            break;
        }
    }
    let Some(text) = final_text else {
        anyhow::bail!("repo-ci subagent completed without a final agent message");
    };
    parse_json_payload(&text)
}

async fn run_repo_ci_learning_subagent_json<T>(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    prompt: String,
    schema: serde_json::Value,
) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    run_repo_ci_read_only_subagent_json(
        sess,
        turn_context,
        ModelRouterSource::Module("repo_ci.learn"),
        SubAgentSource::Other("repo_ci_learn".to_string()),
        prompt,
        schema,
    )
    .await
}

async fn run_repo_ci_read_only_subagent_json<T>(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_router_source: ModelRouterSource,
    subagent_source: SubAgentSource,
    prompt: String,
    schema: serde_json::Value,
) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let mut sub_agent_config =
        repo_ci_phase_config(sess, turn_context, model_router_source, prompt.len());
    if let Err(err) = sub_agent_config
        .web_search_mode
        .set(WebSearchMode::Disabled)
    {
        warn!("failed to disable web search for repo-ci learning subagent: {err}");
    }
    sub_agent_config.permissions.approval_policy =
        crate::config::Constrained::allow_only(AskForApproval::Never);
    sub_agent_config.permissions.sandbox_policy =
        crate::config::Constrained::allow_only(SandboxPolicy::new_read_only_policy());
    let _ = sub_agent_config.mcp_servers.set(HashMap::new());
    let codex = run_codex_thread_one_shot(
        sub_agent_config,
        Arc::clone(&sess.services.auth_manager),
        Arc::clone(&sess.services.models_manager),
        vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }],
        Arc::clone(sess),
        Arc::clone(turn_context),
        CancellationToken::new(),
        subagent_source,
        Some(schema),
        None,
    )
    .await?;
    let mut final_text = None;
    while let Ok(event) = codex.next_event().await {
        if let EventMsg::TurnComplete(turn_complete) = event.msg {
            final_text = turn_complete.last_agent_message;
            break;
        }
    }
    let Some(text) = final_text else {
        anyhow::bail!("repo-ci read-only subagent completed without a final agent message");
    };
    parse_json_payload(&text)
}

fn repo_ci_phase_config(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    model_router_source: ModelRouterSource,
    prompt_bytes: usize,
) -> crate::config::Config {
    let available_models = available_router_models(&sess.services.models_manager);
    repo_ci_phase_config_from_base(
        turn_context.config.as_ref().clone(),
        model_router_source,
        prompt_bytes,
        &available_models,
    )
}

fn repo_ci_phase_config_from_base(
    mut config: crate::config::Config,
    model_router_source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
) -> crate::config::Config {
    if let Err(err) = apply_model_router(
        &mut config,
        model_router_source,
        prompt_bytes,
        available_models,
    ) {
        warn!("failed to apply repo CI model router: {err}");
    }
    config
}

fn parse_json_payload<T>(text: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = text.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);
    Ok(serde_json::from_str(json_text)?)
}

fn targeted_review_prompt(
    config: &EffectiveRepoCiConfig,
    snapshot: &codex_repo_ci::BranchDiffSnapshot,
) -> String {
    let selected = config
        .review_issue_types
        .iter()
        .copied()
        .map(repo_ci_issue_type_slug)
        .collect::<Vec<_>>();
    let excluded = all_repo_ci_issue_types()
        .into_iter()
        .filter(|issue_type| !config.review_issue_types.contains(issue_type))
        .map(repo_ci_issue_type_slug)
        .collect::<Vec<_>>();
    format!(
        "Review the current branch changes and return strict JSON only.\n\nReview scope:\n{}\n\nIn scope issue types:\n- {}\n\nExplicitly out of scope issue types:\n- {}\n\nRules:\n- Review the whole branch diff, not only the latest turn or uncommitted files.\n- Put actionable issues from the selected scope in findings.\n- Put real issues excluded only by the issue-type filter in disregardedFindings with a short reason.\n- Do not duplicate an issue between findings and disregardedFindings.\n- Do not expand into every possible review category.\n- Prefer absolute file paths in findings and disregardedFindings.\n- Use locationHint only when you cannot provide a specific file path.\n- Inspect the workspace as needed before answering.\n\nBranch changed paths:\n```text\n{}\n```\n\nBranch diff summary:\n```text\n{}\n```",
        snapshot.scope_description(),
        selected.join("\n- "),
        excluded.join("\n- "),
        if snapshot.changed_paths.is_empty() {
            "(no changed paths recorded)".to_string()
        } else {
            format_changed_paths_for_prompt(&snapshot.changed_paths)
        },
        truncate_middle(&snapshot.diff_summary, 4_000),
    )
}

fn review_fix_prompt(group: &RepoCiFixGroup) -> String {
    let owned_paths = if group.owned_paths.is_empty() {
        "(no specific file path was available; stay within the described module/location)"
            .to_string()
    } else {
        group
            .owned_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let findings = bounded_review_fix_findings(group);
    format!(
        "Fix the scoped repo CI review findings below.\n\nYou are not alone in the codebase. Do not revert edits made by others, and adjust to concurrent changes if needed.\nOnly edit the owned paths for this worker:\n```text\n{owned_paths}\n```\n\nFindings:\n{findings}\n\nRun only targeted checks; skip full test suites, which repo-ci runs afterward.\n\nAfter applying fixes, return strict JSON with a short summary and touchedFiles."
    )
}

fn bounded_review_fix_findings(group: &RepoCiFixGroup) -> String {
    let mut output = String::new();
    let mut omitted = group.findings.len().saturating_sub(MAX_FIX_WORKER_FINDINGS);
    for finding in group.findings.iter().take(MAX_FIX_WORKER_FINDINGS) {
        let location = finding.location();
        let rendered = format!(
            "- [{}] {}\n{}\nLocation: {}",
            repo_ci_issue_type_slug(finding.issue_type),
            truncate_middle(&finding.title, MAX_FIX_WORKER_FINDING_TITLE_BYTES),
            truncate_middle(&finding.body, MAX_FIX_WORKER_FINDING_BODY_BYTES),
            truncate_middle(&location, MAX_FIX_WORKER_FINDING_LOCATION_BYTES)
        );
        let separator_len = if output.is_empty() { 0 } else { 2 };
        if output.len() + separator_len + rendered.len() > MAX_FIX_WORKER_FINDINGS_BYTES {
            omitted += 1;
            continue;
        }
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&rendered);
    }
    if omitted > 0 {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!("... omitted {omitted} additional finding(s) ..."));
    }
    if output.is_empty() {
        "(no findings were included)".to_string()
    } else {
        output
    }
}

fn review_output_schema() -> serde_json::Value {
    let issue_types = all_repo_ci_issue_types()
        .into_iter()
        .map(repo_ci_issue_type_slug)
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "issueType": {
                            "type": "string",
                            "enum": issue_types
                        },
                        "absoluteFilePath": { "type": ["string", "null"] },
                        "locationHint": { "type": ["string", "null"] }
                    },
                    "required": ["title", "body", "issueType", "absoluteFilePath", "locationHint"]
                }
            },
            "disregardedFindings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "issueType": {
                            "type": "string",
                            "enum": all_repo_ci_issue_types().into_iter().map(repo_ci_issue_type_slug).collect::<Vec<_>>()
                        },
                        "absoluteFilePath": { "type": ["string", "null"] },
                        "locationHint": { "type": ["string", "null"] },
                        "reason": { "type": "string" }
                    },
                    "required": ["title", "body", "issueType", "absoluteFilePath", "locationHint", "reason"]
                }
            }
        },
        "required": ["summary", "findings", "disregardedFindings"]
    })
}

fn review_fix_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string" },
            "touchedFiles": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["summary", "touchedFiles"]
    })
}

fn review_findings_prompt(review: &RepoCiReviewOutput) -> ResponseItem {
    let findings = bounded_review_findings(review);
    ContextualUserFragment::into(RepoCiFollowup::new(format!(
        "Repo CI targeted review found scoped issues that still need fixes.\n\nSummary: {}\n\nFindings:\n{}",
        truncate_middle(&review.summary, MAX_FOLLOWUP_REVIEW_SUMMARY_BYTES),
        findings
    )))
}

fn bounded_review_findings(review: &RepoCiReviewOutput) -> String {
    let mut output = String::new();
    let mut omitted = 0usize;
    for finding in &review.findings {
        let rendered = format!(
            "- [{}] {}: {}\n  {}",
            repo_ci_issue_type_slug(finding.issue_type),
            finding.title,
            finding.location(),
            finding.body
        );
        let separator_len = usize::from(!output.is_empty());
        if output.len() + separator_len + rendered.len() > MAX_FOLLOWUP_FINDINGS_BYTES {
            omitted += 1;
            continue;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&rendered);
    }
    if omitted > 0 {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("... omitted {omitted} additional finding(s) ..."));
    }
    if output.is_empty() {
        "(no findings were included)".to_string()
    } else {
        output
    }
}

fn aggregate_worker_summary(outputs: &[RepoCiFixWorkerOutput]) -> String {
    if outputs.is_empty() {
        return "Repo CI fix workers completed without summaries.".to_string();
    }
    let mut touched_files = BTreeSet::new();
    let summaries = outputs
        .iter()
        .map(|output| {
            for file in &output.touched_files {
                touched_files.insert(file.clone());
            }
            output.summary.clone()
        })
        .collect::<Vec<_>>();
    format!(
        "Repo CI fix workers applied {} grouped fix(es) touching {} file(s): {}",
        outputs.len(),
        touched_files.len(),
        summaries.join(" | ")
    )
}

fn all_repo_ci_issue_types() -> Vec<RepoCiIssueType> {
    vec![
        RepoCiIssueType::Correctness,
        RepoCiIssueType::Reliability,
        RepoCiIssueType::Performance,
        RepoCiIssueType::Scalability,
        RepoCiIssueType::Security,
        RepoCiIssueType::Maintainability,
        RepoCiIssueType::Testability,
        RepoCiIssueType::Observability,
        RepoCiIssueType::Compatibility,
        RepoCiIssueType::UxConfigCli,
    ]
}

fn repo_ci_issue_type_slug(issue_type: RepoCiIssueType) -> &'static str {
    match issue_type {
        RepoCiIssueType::Correctness => "correctness",
        RepoCiIssueType::Reliability => "reliability",
        RepoCiIssueType::Performance => "performance",
        RepoCiIssueType::Scalability => "scalability",
        RepoCiIssueType::Security => "security",
        RepoCiIssueType::Maintainability => "maintainability",
        RepoCiIssueType::Testability => "testability",
        RepoCiIssueType::Observability => "observability",
        RepoCiIssueType::Compatibility => "compatibility",
        RepoCiIssueType::UxConfigCli => "ux-config-cli",
    }
}

fn effective_config(turn_context: &TurnContext) -> Option<EffectiveRepoCiConfig> {
    if !turn_context
        .config
        .features
        .enabled(codex_features::Feature::RepoCi)
    {
        return None;
    }
    let cwd = &turn_context.cwd;
    let scoped_config = turn_context
        .config
        .repo_ci
        .as_ref()
        .and_then(|repo_ci| scoped_repo_ci_config(cwd, repo_ci));
    let review_issue_types = turn_context
        .repo_ci_issue_types
        .clone()
        .or_else(|| turn_context.config.repo_ci_issue_types.clone())
        .or_else(|| {
            scoped_config
                .as_ref()
                .and_then(|scope| scope.review_issue_types.clone())
        })
        .or_else(|| inferred_issue_types(turn_context))
        .unwrap_or_else(codex_repo_ci::default_issue_types);
    let review_rounds = turn_context
        .repo_ci_review_rounds
        .or(turn_context.config.repo_ci_review_rounds)
        .or_else(|| {
            scoped_config
                .as_ref()
                .and_then(|scope| scope.max_review_fix_rounds)
        });
    let long_ci = turn_context
        .repo_ci_long_ci
        .or(turn_context.config.repo_ci_long_ci)
        .or_else(|| scoped_config.as_ref().and_then(|scope| scope.long_ci))
        .unwrap_or(false);
    if let Some(mode) = turn_context.repo_ci_session_mode {
        return EffectiveRepoCiConfig::from_session_mode(
            mode,
            scoped_config.as_ref(),
            review_issue_types,
            review_rounds,
            long_ci,
        );
    }
    if let Some(mode) = turn_context.config.repo_ci_session_mode {
        return EffectiveRepoCiConfig::from_session_mode(
            mode,
            scoped_config.as_ref(),
            review_issue_types,
            review_rounds,
            long_ci,
        );
    }
    scoped_config
        .as_ref()
        .and_then(|scope| EffectiveRepoCiConfig::from_scope(scope, review_issue_types, long_ci))
}

fn scoped_repo_ci_config(
    cwd: &Path,
    repo_ci: &codex_config::config_toml::RepoCiToml,
) -> Option<RepoCiScopeToml> {
    let scope = most_specific_directory_scope(cwd, &repo_ci.directories)
        .or_else(|| github_repo(cwd).and_then(|repo| repo_ci.github_repos.get(&repo)))
        .or_else(|| github_org(cwd).and_then(|org| repo_ci.github_orgs.get(&org)));
    match (repo_ci.defaults.as_ref(), scope) {
        (Some(defaults), Some(scope)) => Some(merge_repo_ci_scope(defaults, scope)),
        (None, Some(scope)) => Some(scope.clone()),
        (Some(defaults), None) => Some(defaults.clone()),
        (None, None) => None,
    }
}

fn merge_repo_ci_scope(defaults: &RepoCiScopeToml, scope: &RepoCiScopeToml) -> RepoCiScopeToml {
    RepoCiScopeToml {
        enabled: scope.enabled.or(defaults.enabled),
        automation: scope.automation.or(defaults.automation),
        local_test_time_budget_sec: scope
            .local_test_time_budget_sec
            .or(defaults.local_test_time_budget_sec),
        long_ci: scope.long_ci.or(defaults.long_ci),
        max_local_fix_rounds: scope.max_local_fix_rounds.or(defaults.max_local_fix_rounds),
        max_remote_fix_rounds: scope
            .max_remote_fix_rounds
            .or(defaults.max_remote_fix_rounds),
        review_issue_types: scope
            .review_issue_types
            .clone()
            .or_else(|| defaults.review_issue_types.clone()),
        max_review_fix_rounds: scope
            .max_review_fix_rounds
            .or(defaults.max_review_fix_rounds),
    }
}

fn session_mode_to_automation(mode: RepoCiSessionMode) -> RepoCiAutomationToml {
    match mode {
        RepoCiSessionMode::Off => RepoCiAutomationToml::LocalAndRemote,
        RepoCiSessionMode::Local => RepoCiAutomationToml::Local,
        RepoCiSessionMode::Remote => RepoCiAutomationToml::Remote,
        RepoCiSessionMode::LocalAndRemote => RepoCiAutomationToml::LocalAndRemote,
    }
}

fn most_specific_directory_scope<'a>(
    cwd: &Path,
    scopes: &'a BTreeMap<String, RepoCiScopeToml>,
) -> Option<&'a RepoCiScopeToml> {
    scopes
        .iter()
        .filter_map(|(raw_path, scope)| {
            let path = expand_home(raw_path);
            cwd.starts_with(&path)
                .then_some((path.components().count(), scope))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, scope)| scope)
}

fn expand_home(raw_path: &str) -> PathBuf {
    if raw_path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(suffix) = raw_path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(suffix);
    }
    PathBuf::from(raw_path)
}

fn github_org(cwd: &Path) -> Option<String> {
    github_remote_parts(cwd).map(|(org, _)| org)
}

fn github_repo(cwd: &Path) -> Option<String> {
    github_remote_parts(cwd).map(|(org, repo)| format!("{org}/{repo}"))
}

fn github_remote_parts(cwd: &Path) -> Option<(String, String)> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_remote(String::from_utf8_lossy(&output.stdout).trim())
}

#[cfg(test)]
fn parse_github_org(remote: &str) -> Option<String> {
    parse_github_remote(remote).map(|(org, _)| org)
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let marker = "github.com";
    let index = remote.find(marker)?;
    let after_host = remote[index + marker.len()..]
        .trim_start_matches(':')
        .trim_start_matches('/');
    let mut parts = after_host.split('/');
    let org = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches(".git").to_string();
    Some((org, repo))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalRepoCiOutcome {
    Skipped,
    Passed,
    Failed { output: String },
}

fn run_local_repo_ci(
    codex_home: &Path,
    cwd: &Path,
    config: &EffectiveRepoCiConfig,
    cancellation: codex_repo_ci::RepoCiCancellation,
) -> Result<LocalRepoCiOutcome> {
    if !config.local_enabled() {
        return Ok(LocalRepoCiOutcome::Skipped);
    }
    let run_mode = if config.long_ci {
        codex_repo_ci::RunMode::Full
    } else {
        codex_repo_ci::RunMode::Fast
    };
    let runner_label = if config.long_ci {
        "local full runner"
    } else {
        "local fast runner"
    };
    let artifact = codex_repo_ci::run_capture_persisted_with_cancellation(
        codex_home,
        cwd,
        run_mode,
        cancellation,
    )?;
    if artifact.status == codex_repo_ci::RepoCiRunArtifactStatus::Passed {
        Ok(LocalRepoCiOutcome::Passed)
    } else {
        Ok(LocalRepoCiOutcome::Failed {
            output: format_run_output(runner_label, &artifact),
        })
    }
}

fn automation_to_repo_ci(automation: RepoCiAutomationToml) -> codex_repo_ci::AutomationMode {
    match automation {
        RepoCiAutomationToml::Local => codex_repo_ci::AutomationMode::Local,
        RepoCiAutomationToml::Remote => codex_repo_ci::AutomationMode::Remote,
        RepoCiAutomationToml::LocalAndRemote => codex_repo_ci::AutomationMode::LocalAndRemote,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteRepoCiOutcome {
    Skipped(String),
    Passed {
        prepared_commit: Option<codex_repo_ci::RemoteCommitApplied>,
    },
    Failed {
        output: String,
        classification: FailureClassification,
    },
}

fn run_remote_repo_ci(
    cwd: &Path,
    workflow: &codex_repo_ci::RemoteRepoCiWorkflow,
    commit_decision: Option<&codex_repo_ci::RemoteCommitDecision>,
    owned_paths: &[String],
) -> Result<RemoteRepoCiOutcome> {
    let run = codex_repo_ci::run_started_remote_workflow_with_commit_decision(
        cwd,
        workflow,
        commit_decision,
        owned_paths,
    )?;
    match run.outcome {
        codex_repo_ci::RemoteRepoCiWorkflowOutcome::Skipped(reason) => {
            Ok(RemoteRepoCiOutcome::Skipped(reason))
        }
        codex_repo_ci::RemoteRepoCiWorkflowOutcome::Passed => Ok(RemoteRepoCiOutcome::Passed {
            prepared_commit: run.prepared_commit,
        }),
        codex_repo_ci::RemoteRepoCiWorkflowOutcome::Failed {
            watch_status,
            checks,
        } => {
            if checks.is_empty() {
                return Ok(RemoteRepoCiOutcome::Failed {
                    output: format!(
                        "GitHub PR checks failed with {watch_status}, but `gh pr checks --json` returned no checks."
                    ),
                    classification: FailureClassification::Unknown,
                });
            }
            let failed = checks
                .iter()
                .filter(|check| check.bucket.as_deref() == Some("fail") || check.state == "FAILURE")
                .collect::<Vec<_>>();
            if failed.is_empty() {
                return Ok(RemoteRepoCiOutcome::Passed {
                    prepared_commit: run.prepared_commit,
                });
            }
            let classification = classify_remote_failure(checks.len(), &failed);
            Ok(RemoteRepoCiOutcome::Failed {
                output: format!(
                    "classification: {classification:?}\n\n{}",
                    format_remote_checks(&checks)
                ),
                classification,
            })
        }
    }
}

fn format_remote_checks(checks: &[codex_repo_ci::RemoteRepoCiCheck]) -> String {
    checks
        .iter()
        .map(|check| {
            format!(
                "{}: {} ({})",
                check.name,
                check.bucket.as_deref().unwrap_or("unknown"),
                check.link.as_deref().unwrap_or("no link")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_run_output(label: &str, artifact: &codex_repo_ci::RepoCiRunArtifact) -> String {
    let step_output = artifact
        .steps
        .iter()
        .map(|step| format!("{} {:?} {:?}", step.id, step.status, step.exit_code))
        .collect::<Vec<_>>()
        .join("\n");
    let failed_steps = artifact
        .steps
        .iter()
        .filter(|step| step.status == codex_repo_ci::RepoCiStepRunStatus::Failed)
        .map(|step| step.id.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let error_output = if artifact.stderr.trim().is_empty() {
        artifact.stdout.as_str()
    } else {
        artifact.stderr.as_str()
    };
    truncate_middle(
        &format!(
            "{label}\n\nartifact_id: {}\nfailed_steps: {}\n\nsteps:\n{step_output}\n\nerror_output:\n{}",
            artifact.artifact_id,
            if failed_steps.is_empty() {
                "(unknown)"
            } else {
                failed_steps.as_str()
            },
            truncate_middle(error_output, MAX_FOLLOWUP_OUTPUT_BYTES),
        ),
        MAX_OUTPUT_BYTES,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureClassification {
    Related,
    Unrelated,
    WholeSuite,
    Unknown,
}

impl FailureClassification {
    fn as_str(self) -> &'static str {
        match self {
            FailureClassification::Related => "related",
            FailureClassification::Unrelated => "unrelated",
            FailureClassification::WholeSuite => "whole_suite",
            FailureClassification::Unknown => "unknown",
        }
    }
}

fn classify_remote_failure(
    total_checks: usize,
    failed: &[&codex_repo_ci::RemoteRepoCiCheck],
) -> FailureClassification {
    if failed.is_empty() {
        return FailureClassification::Unknown;
    }
    if failed.len() == total_checks {
        return FailureClassification::WholeSuite;
    }
    if failed.iter().all(|check| {
        let name = check.name.to_ascii_lowercase();
        name.contains("unrelated") || name.contains("infra")
    }) {
        return FailureClassification::Unrelated;
    }
    FailureClassification::Related
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TriageConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct TriageResult {
    classification: FailureClassification,
    confidence: TriageConfidence,
    summary: String,
    failed_steps: Vec<String>,
    #[serde(default)]
    model_used: Option<String>,
}

impl TriageResult {
    fn deterministic(classification: FailureClassification, summary: impl Into<String>) -> Self {
        let confidence = match classification {
            FailureClassification::Unrelated | FailureClassification::WholeSuite => {
                TriageConfidence::High
            }
            FailureClassification::Related | FailureClassification::Unknown => {
                TriageConfidence::Low
            }
        };
        Self {
            classification,
            confidence,
            summary: summary.into(),
            failed_steps: Vec::new(),
            model_used: None,
        }
    }

    fn should_ignore(&self) -> bool {
        self.classification == FailureClassification::Unrelated
            && self.confidence == TriageConfidence::High
    }
}

struct TriageInput<'a> {
    kind: &'a str,
    output: &'a str,
    changed_paths: &'a [String],
    diff_summary: &'a str,
    deterministic_classification: FailureClassification,
}

async fn triage_failure(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: TriageInput<'_>,
) -> TriageResult {
    send_status(
        sess,
        turn_context,
        RepoCiPhase::Triage,
        RepoCiState::Started,
        if input.kind == "remote" {
            RepoCiScope::Remote
        } else {
            RepoCiScope::Local
        },
        None,
        None,
        format!("Repo CI {} failure triage started.", input.kind),
    )
    .await;

    match run_model_triage(sess, turn_context, &input).await {
        Ok(mut triage) => {
            if triage.classification == FailureClassification::WholeSuite
                || input.deterministic_classification == FailureClassification::WholeSuite
            {
                triage.classification = FailureClassification::WholeSuite;
            }
            send_status(
                sess,
                turn_context,
                RepoCiPhase::Triage,
                RepoCiState::Passed,
                if input.kind == "remote" {
                    RepoCiScope::Remote
                } else {
                    RepoCiScope::Local
                },
                None,
                None,
                format!(
                    "Repo CI triage classified {} failure as {}.",
                    input.kind,
                    triage.classification.as_str(),
                ),
            )
            .await;
            return triage;
        }
        Err(err) => {
            warn!("Repo CI triage model failed: {err:#}");
        }
    }

    let summary = match input.deterministic_classification {
        FailureClassification::WholeSuite => {
            "all reported CI checks failed; treating as a whole-suite failure"
        }
        FailureClassification::Unrelated => {
            "failed check names look unrelated or infrastructure-only"
        }
        FailureClassification::Related => "failed check names look related to branch validation",
        FailureClassification::Unknown => "no model triage result was available",
    };
    TriageResult::deterministic(input.deterministic_classification, summary)
}

async fn run_model_triage(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    input: &TriageInput<'_>,
) -> Result<TriageResult> {
    let triage_prompt = triage_prompt_text(input);
    let policy_config = repo_ci_phase_config(
        sess,
        turn_context,
        ModelRouterSource::Module("repo_ci.triage"),
        triage_prompt.len(),
    );
    let model = policy_config
        .model
        .clone()
        .unwrap_or_else(|| turn_context.model_info.slug.clone());
    let model_info = if policy_config.model.as_deref()
        != Some(turn_context.model_info.slug.as_str())
        || policy_config.model_provider_id != turn_context.config.model_provider_id
    {
        sess.services
            .models_manager
            .get_model_info(&model, &policy_config.to_models_manager_config())
            .await
    } else {
        turn_context.model_info.clone()
    };
    let effort = policy_config
        .model_reasoning_effort
        .or(turn_context.reasoning_effort)
        .or(model_info.default_reasoning_level);

    let routed_auth_manager = auth_manager_for_config(&policy_config, &sess.services.auth_manager);
    let routed_model_client = sess.services.model_client.with_provider_info(
        policy_config.model_provider.clone(),
        Some(routed_auth_manager),
    );
    let mut client_session = routed_model_client.new_session();
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: triage_prompt,
            }],
            phase: None,
        }],
        base_instructions: BaseInstructions {
            text: TRIAGE_BASE_INSTRUCTIONS.to_string(),
        },
        output_schema: Some(triage_output_schema()),
        output_schema_strict: true,
        ..Default::default()
    };
    let turn_metadata_header = turn_context.turn_metadata_state.current_header_value();
    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            &turn_context.session_telemetry,
            effort,
            turn_context.reasoning_summary,
            policy_config.service_tier,
            turn_metadata_header.as_deref(),
            &InferenceTraceContext::disabled(),
        )
        .await?;
    let mut output = String::new();
    while let Some(event) = stream.next().await {
        match event? {
            ResponseEvent::OutputTextDelta(delta) => output.push_str(&delta),
            ResponseEvent::OutputItemDone(item) => append_response_item_text(&mut output, &item),
            ResponseEvent::Completed { .. } => break,
            ResponseEvent::Created
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::ServerModel(_)
            | ResponseEvent::ModelVerifications(_)
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::ToolCallInputDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::RateLimits(_)
            | ResponseEvent::ModelsEtag(_) => {}
        }
    }
    let mut triage = parse_triage_result(&output)?;
    triage.model_used = Some(model_info.slug);
    Ok(triage)
}

fn triage_prompt_text(input: &TriageInput<'_>) -> String {
    let changed_paths = if input.changed_paths.is_empty() {
        "(no changed paths recorded)".to_string()
    } else {
        format_changed_paths_for_prompt(input.changed_paths)
    };
    format!(
        "Classify this repo CI {kind} failure.\n\nRules:\n- Return JSON only.\n- classification must be one of related, unrelated, whole_suite, unknown.\n- Use unrelated only when the failure is clearly not caused by the current branch.\n- Use whole_suite if all or nearly all checks failed or the output indicates broad infrastructure failure.\n- Use unknown when evidence is insufficient.\n\nDeterministic initial classification: {classification}\n\nChanged paths:\n```text\n{changed_paths}\n```\n\nDiff summary:\n```text\n{}\n```\n\nFailure output:\n```text\n{}\n```",
        truncate_middle(input.diff_summary, 4_000),
        truncate_middle(input.output, MAX_OUTPUT_BYTES),
        kind = input.kind,
        classification = input.deterministic_classification.as_str(),
    )
}

fn triage_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "classification": {
                "type": "string",
                "enum": ["related", "unrelated", "whole_suite", "unknown"]
            },
            "confidence": {
                "type": "string",
                "enum": ["low", "medium", "high"]
            },
            "summary": { "type": "string" },
            "failed_steps": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["classification", "confidence", "summary", "failed_steps"]
    })
}

fn append_response_item_text(output: &mut String, item: &ResponseItem) {
    if let ResponseItem::Message { content, .. } = item {
        for content_item in content {
            match content_item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    output.push_str(text);
                }
                ContentItem::InputImage { .. } => {}
            }
        }
    }
}

fn parse_triage_result(output: &str) -> Result<TriageResult> {
    let trimmed = output.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);
    let mut triage: TriageResult = serde_json::from_str(json_text)?;
    triage.model_used = None;
    Ok(triage)
}

fn repair_prompt(
    kind: &str,
    output: &str,
    changed_paths: &[String],
    diff_summary: &str,
    triage: &TriageResult,
) -> ResponseItem {
    let changed_paths = if changed_paths.is_empty() {
        "(no changed paths recorded)".to_string()
    } else {
        truncate_middle(&changed_paths.join("\n"), MAX_FOLLOWUP_CHANGED_PATHS_BYTES)
    };
    let text = format!(
        "Repo CI {kind} checks failed after your changes.\n\nTriage classification: {} ({:?} confidence)\nTriage summary: {}\n\nChanged paths:\n```text\n{changed_paths}\n```\n\nDiff summary:\n```text\n{}\n```\n\nIf the failure is related or uncertain, fix the code, then let repo CI run again. Do not edit code for clearly unrelated failures.\n\nFailure output:\n```text\n{}\n```",
        triage.classification.as_str(),
        triage.confidence,
        truncate_middle(&triage.summary, MAX_FOLLOWUP_TRIAGE_SUMMARY_BYTES),
        truncate_middle(diff_summary, MAX_FOLLOWUP_DIFF_BYTES),
        truncate_middle(output, MAX_FOLLOWUP_OUTPUT_BYTES)
    );
    ContextualUserFragment::into(RepoCiFollowup::new(text))
}

fn format_changed_paths_for_prompt(changed_paths: &[String]) -> String {
    truncate_middle(&changed_paths.join("\n"), MAX_CHANGED_PATHS_BYTES)
}

fn truncate_middle(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let half = max_bytes / 2;
    let start_end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= half)
        .last()
        .unwrap_or(0);
    let end_start = value
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= value.len().saturating_sub(half))
        .unwrap_or(value.len());
    format!(
        "{}\n\n... omitted {} bytes ...\n\n{}",
        &value[..start_end],
        end_start.saturating_sub(start_end),
        &value[end_start..]
    )
}

async fn send_review_resolution_summary(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    tracker: &RepoCiReviewIssueTracker,
    state: RepoCiState,
    attempt: Option<u8>,
    max_attempts: Option<u8>,
) {
    if let Some(message) = tracker.summary_message() {
        send_status(
            sess,
            turn_context,
            RepoCiPhase::Triage,
            state,
            RepoCiScope::Local,
            attempt,
            max_attempts,
            message,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_status(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    phase: RepoCiPhase,
    state: RepoCiState,
    scope: RepoCiScope,
    attempt: Option<u8>,
    max_attempts: Option<u8>,
    message: String,
) {
    warn!("{message}");
    sess.send_event(
        turn_context,
        EventMsg::RepoCiStatus(RepoCiStatusEvent {
            phase,
            state,
            scope,
            attempt,
            max_attempts,
            message,
        }),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use codex_config::config_toml::ModelRouterCandidateToml;
    use codex_config::config_toml::ModelRouterToml;
    use codex_config::config_toml::RepoCiToml;
    use codex_protocol::config_types::ServiceTier;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn parses_github_org_from_common_remote_forms() {
        assert_eq!(
            parse_github_org("git@github.com:openai/codex.git"),
            Some("openai".to_string())
        );
        assert_eq!(
            parse_github_org("https://github.com/dkropachev/codex"),
            Some("dkropachev".to_string())
        );
    }

    #[test]
    fn directory_scope_uses_most_specific_match() {
        let mut repo_ci = RepoCiToml::default();
        repo_ci.directories.insert(
            "/tmp/repo".to_string(),
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Local),
                ..Default::default()
            },
        );
        repo_ci.directories.insert(
            "/tmp/repo/nested".to_string(),
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Remote),
                ..Default::default()
            },
        );

        let scope =
            most_specific_directory_scope(Path::new("/tmp/repo/nested/src"), &repo_ci.directories)
                .expect("scope");
        assert_eq!(scope.automation, Some(RepoCiAutomationToml::Remote));
    }

    #[test]
    fn scoped_config_prefers_directory_then_repo_then_org_then_defaults() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir(&repo_root).expect("create repo");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_root)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["remote", "add", "origin", "git@github.com:openai/codex.git"])
            .current_dir(&repo_root)
            .output()
            .expect("git remote add");

        let mut repo_ci = RepoCiToml {
            defaults: Some(RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::LocalAndRemote),
                ..Default::default()
            }),
            ..Default::default()
        };
        repo_ci.github_orgs.insert(
            "openai".to_string(),
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Remote),
                ..Default::default()
            },
        );
        repo_ci.github_repos.insert(
            "openai/codex".to_string(),
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Local),
                ..Default::default()
            },
        );
        repo_ci.directories.insert(
            repo_root.to_string_lossy().to_string(),
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Remote),
                ..Default::default()
            },
        );

        let scope = scoped_repo_ci_config(&repo_root, &repo_ci).expect("scope");
        assert_eq!(scope.automation, Some(RepoCiAutomationToml::Remote));

        repo_ci.directories.clear();
        let scope = scoped_repo_ci_config(&repo_root, &repo_ci).expect("scope");
        assert_eq!(scope.automation, Some(RepoCiAutomationToml::Local));

        repo_ci.github_repos.clear();
        let scope = scoped_repo_ci_config(&repo_root, &repo_ci).expect("scope");
        assert_eq!(scope.automation, Some(RepoCiAutomationToml::Remote));

        repo_ci.github_orgs.clear();
        let scope = scoped_repo_ci_config(&repo_root, &repo_ci).expect("scope");
        assert_eq!(scope.automation, Some(RepoCiAutomationToml::LocalAndRemote));
    }

    #[test]
    fn scoped_config_merges_missing_fields_from_defaults() {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir(&repo_root).expect("create repo");
        Command::new("git")
            .args(["init"])
            .current_dir(&repo_root)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["remote", "add", "origin", "git@github.com:openai/codex.git"])
            .current_dir(&repo_root)
            .output()
            .expect("git remote add");

        let mut repo_ci = RepoCiToml {
            defaults: Some(RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Remote),
                max_local_fix_rounds: Some(7),
                ..Default::default()
            }),
            ..Default::default()
        };
        repo_ci.github_repos.insert(
            "openai/codex".to_string(),
            RepoCiScopeToml {
                review_issue_types: Some(vec![RepoCiIssueType::Security]),
                ..Default::default()
            },
        );

        let scope = scoped_repo_ci_config(&repo_root, &repo_ci).expect("scope");

        assert_eq!(
            scope,
            RepoCiScopeToml {
                enabled: Some(true),
                automation: Some(RepoCiAutomationToml::Remote),
                max_local_fix_rounds: Some(7),
                review_issue_types: Some(vec![RepoCiIssueType::Security]),
                ..Default::default()
            }
        );
        let config = EffectiveRepoCiConfig::from_scope(
            &scope,
            scope.review_issue_types.clone().expect("issue types"),
            false,
        )
        .expect("merged scope should be enabled");
        assert_eq!(config.automation, RepoCiAutomationToml::Remote);
    }

    #[test]
    fn session_mode_overrides_disabled_scope() {
        let scope = RepoCiScopeToml {
            enabled: Some(false),
            automation: Some(RepoCiAutomationToml::Remote),
            max_local_fix_rounds: Some(7),
            ..Default::default()
        };

        let config = EffectiveRepoCiConfig::from_session_mode(
            RepoCiSessionMode::Local,
            Some(&scope),
            codex_repo_ci::default_issue_types(),
            None,
            false,
        )
        .expect("session override enables repo ci");

        assert_eq!(config.automation, RepoCiAutomationToml::Local);
        assert_eq!(config.max_local_fix_rounds, 7);
    }

    #[test]
    fn review_empty_issue_types_disable_review_phase() {
        let config = EffectiveRepoCiConfig {
            automation: RepoCiAutomationToml::Local,
            local_test_time_budget_sec: 300,
            long_ci: false,
            max_local_fix_rounds: 3,
            max_remote_fix_rounds: 2,
            review_issue_types: Vec::new(),
            max_review_fix_rounds: 2,
        };

        assert!(!config.review_enabled());
    }

    #[test]
    fn review_zero_rounds_disable_review_phase() {
        let config = EffectiveRepoCiConfig {
            automation: RepoCiAutomationToml::Local,
            local_test_time_budget_sec: 300,
            long_ci: false,
            max_local_fix_rounds: 3,
            max_remote_fix_rounds: 2,
            review_issue_types: codex_repo_ci::default_issue_types(),
            max_review_fix_rounds: 0,
        };

        assert!(!config.review_enabled());
    }

    #[test]
    fn off_session_mode_disables_repo_ci() {
        let scope = RepoCiScopeToml {
            enabled: Some(true),
            automation: Some(RepoCiAutomationToml::LocalAndRemote),
            ..Default::default()
        };

        assert_eq!(
            EffectiveRepoCiConfig::from_session_mode(
                RepoCiSessionMode::Off,
                Some(&scope),
                codex_repo_ci::default_issue_types(),
                None,
                false,
            ),
            None
        );
    }

    #[test]
    fn truncate_middle_preserves_utf8_boundaries() {
        let truncated = truncate_middle("prefix 😎 middle 😎 suffix", 12);
        assert!(truncated.contains("omitted"));
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn format_changed_paths_for_prompt_caps_large_lists() {
        let changed_paths = (0..2_000)
            .map(|index| format!("src/generated/file_{index}.rs"))
            .collect::<Vec<_>>();

        let formatted = format_changed_paths_for_prompt(&changed_paths);

        assert!(formatted.len() < changed_paths.join("\n").len());
        assert!(formatted.contains("omitted"));
    }

    #[test]
    fn repair_prompt_is_bounded_contextual_followup() {
        let changed_paths = (0..500)
            .map(|index| format!("src/generated/file_{index}.rs"))
            .collect::<Vec<_>>();
        let triage =
            TriageResult::deterministic(FailureClassification::Related, "triage ".repeat(1_000));

        let prompt = response_item_text(&repair_prompt(
            "local",
            &"output ".repeat(10_000),
            &changed_paths,
            &"diff ".repeat(10_000),
            &triage,
        ));

        assert!(prompt.starts_with("<repo_ci_followup>"));
        assert!(prompt.ends_with("</repo_ci_followup>"));
        assert!(prompt.len() < 5_000);
        assert!(prompt.contains("omitted"));
    }

    #[test]
    fn targeted_review_prompt_lists_selected_and_excluded_issue_types() {
        let config = EffectiveRepoCiConfig {
            automation: RepoCiAutomationToml::Local,
            local_test_time_budget_sec: 300,
            long_ci: false,
            max_local_fix_rounds: 3,
            max_remote_fix_rounds: 2,
            review_issue_types: vec![RepoCiIssueType::Correctness, RepoCiIssueType::Security],
            max_review_fix_rounds: 2,
        };
        let snapshot = codex_repo_ci::BranchDiffSnapshot {
            base_ref: Some("origin/main".to_string()),
            merge_base: Some("abc".to_string()),
            changed_paths: vec!["src/main.rs".to_string()],
            diff_summary: "1 file changed".to_string(),
        };

        let prompt = targeted_review_prompt(&config, &snapshot);

        assert!(prompt.contains("Review the whole branch diff"));
        assert!(prompt.contains("Whole branch diff against `origin/main` using merge base `abc`"));
        assert!(prompt.contains("disregardedFindings"));
        assert!(prompt.contains("- correctness"));
        assert!(prompt.contains("- security"));
        for excluded in [
            "reliability",
            "performance",
            "scalability",
            "maintainability",
            "testability",
            "observability",
            "compatibility",
            "ux-config-cli",
        ] {
            assert!(
                prompt.contains(&format!("- {excluded}")),
                "expected excluded issue type in prompt: {excluded}\n{prompt}"
            );
        }
    }

    fn review_finding(title: &str, issue_type: RepoCiIssueType, path: &str) -> RepoCiReviewFinding {
        RepoCiReviewFinding {
            title: title.to_string(),
            body: "body".to_string(),
            issue_type,
            absolute_file_path: Some(PathBuf::from(path)),
            location_hint: None,
        }
    }

    #[test]
    fn review_fix_prompt_caps_findings() {
        let group = RepoCiFixGroup {
            key: "repo".to_string(),
            owned_paths: vec![PathBuf::from("/tmp/repo/src/main.rs")],
            findings: (0..20)
                .map(|index| RepoCiReviewFinding {
                    title: format!("finding {index}"),
                    body: format!("body {index} {}", "x".repeat(2_000)),
                    issue_type: RepoCiIssueType::Correctness,
                    absolute_file_path: Some(PathBuf::from("/tmp/repo/src/main.rs")),
                    location_hint: None,
                })
                .collect(),
        };

        let prompt = review_fix_prompt(&group);

        assert!(prompt.len() < 8_000);
        assert!(prompt.contains("omitted"));
        assert!(prompt.contains("finding 0"));
        assert!(!prompt.contains("finding 19"));
    }

    fn disregarded_finding(
        title: &str,
        issue_type: RepoCiIssueType,
        path: &str,
    ) -> RepoCiDisregardedFinding {
        RepoCiDisregardedFinding {
            title: title.to_string(),
            body: "body".to_string(),
            issue_type,
            absolute_file_path: Some(PathBuf::from(path)),
            location_hint: None,
            reason: "outside configured issue-type filter".to_string(),
        }
    }

    #[test]
    fn review_findings_prompt_is_bounded_contextual_followup() {
        let review = RepoCiReviewOutput {
            summary: "summary ".repeat(1_000),
            findings: (0..50)
                .map(|index| RepoCiReviewFinding {
                    title: format!("finding {index}"),
                    body: "body ".repeat(500),
                    issue_type: RepoCiIssueType::Correctness,
                    absolute_file_path: Some(PathBuf::from(format!("/tmp/repo/src/{index}.rs"))),
                    location_hint: None,
                })
                .collect(),
            disregarded_findings: Vec::new(),
        };

        let prompt = response_item_text(&review_findings_prompt(&review));

        assert!(prompt.starts_with("<repo_ci_followup>"));
        assert!(prompt.ends_with("</repo_ci_followup>"));
        assert!(prompt.len() < 5_000);
        assert!(prompt.contains("omitted"));
    }

    fn response_item_text(item: &ResponseItem) -> String {
        match item {
            ResponseItem::Message { content, .. } => content
                .iter()
                .filter_map(|content| match content {
                    ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                        Some(text.as_str())
                    }
                    ContentItem::InputImage { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }

    #[test]
    fn review_tracker_records_resolved_and_disregarded_filtered_issues() {
        let mut tracker = RepoCiReviewIssueTracker::default();
        let resolved = review_finding(
            "same-day timestamp bounds are rejected",
            RepoCiIssueType::Correctness,
            "/tmp/repo/src/config.rs",
        );
        let still_active = review_finding(
            "retry error hides failure output",
            RepoCiIssueType::Reliability,
            "/tmp/repo/src/retry.rs",
        );
        let disregarded = disregarded_finding(
            "missing debug context",
            RepoCiIssueType::Observability,
            "/tmp/repo/src/logging.rs",
        );
        let selected_issue_type = disregarded_finding(
            "selected issue type should not be disregarded",
            RepoCiIssueType::Correctness,
            "/tmp/repo/src/config.rs",
        );
        let selected_issue_types = [RepoCiIssueType::Correctness, RepoCiIssueType::Reliability];

        tracker.record_review(
            &RepoCiReviewOutput {
                findings: vec![resolved.clone(), still_active.clone()],
                disregarded_findings: vec![disregarded.clone(), selected_issue_type],
                summary: "first pass".to_string(),
            },
            &selected_issue_types,
        );
        tracker.record_review(
            &RepoCiReviewOutput {
                findings: vec![still_active],
                disregarded_findings: Vec::new(),
                summary: "second pass".to_string(),
            },
            &selected_issue_types,
        );

        assert_eq!(
            tracker.resolved.values().cloned().collect::<Vec<_>>(),
            vec![resolved]
        );
        assert_eq!(
            tracker.disregarded.values().cloned().collect::<Vec<_>>(),
            vec![disregarded]
        );
        let summary = tracker.summary_message().expect("summary");
        assert!(summary.contains("Resolved issues:"));
        assert!(summary.contains("same-day timestamp bounds are rejected"));
        assert!(summary.contains("Disregarded by issue-type filter:"));
        assert!(summary.contains("missing debug context"));
        assert!(!summary.contains("selected issue type should not be disregarded"));
    }

    #[test]
    fn review_findings_group_by_absolute_file_path() {
        let findings = vec![
            RepoCiReviewFinding {
                title: "a".to_string(),
                body: "body".to_string(),
                issue_type: RepoCiIssueType::Correctness,
                absolute_file_path: Some(PathBuf::from("/tmp/repo/src/lib.rs")),
                location_hint: None,
            },
            RepoCiReviewFinding {
                title: "b".to_string(),
                body: "body".to_string(),
                issue_type: RepoCiIssueType::Reliability,
                absolute_file_path: Some(PathBuf::from("/tmp/repo/src/lib.rs")),
                location_hint: Some("src".to_string()),
            },
            RepoCiReviewFinding {
                title: "c".to_string(),
                body: "body".to_string(),
                issue_type: RepoCiIssueType::Security,
                absolute_file_path: None,
                location_hint: Some("src/network".to_string()),
            },
        ];

        let groups = group_review_findings(&findings);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "/tmp/repo/src/lib.rs");
        assert_eq!(groups[0].findings.len(), 2);
        assert_eq!(
            groups[0].owned_paths,
            vec![PathBuf::from("/tmp/repo/src/lib.rs")]
        );
        assert_eq!(groups[1].key, "src/network");
        assert!(groups[1].owned_paths.is_empty());
        assert_eq!(groups[1].findings.len(), 1);
    }

    #[test]
    fn parses_git_status_changed_paths() {
        assert_eq!(
            parse_status_paths(b" M src/lib.rs\0?? new file.txt\0"),
            vec!["src/lib.rs".to_string(), "new file.txt".to_string()]
        );
    }

    #[test]
    fn owned_changed_paths_excludes_preexisting_dirty_paths() {
        let initial_snapshot = WorktreeSnapshot {
            digest: "initial".to_string(),
            changed_paths: vec!["preexisting.txt".to_string()],
            diff_summary: String::new(),
        };
        let current_snapshot = WorktreeSnapshot {
            digest: "current".to_string(),
            changed_paths: vec!["preexisting.txt".to_string(), "owned.txt".to_string()],
            diff_summary: String::new(),
        };

        assert_eq!(
            repo_ci_owned_changed_paths(&initial_snapshot, &current_snapshot),
            vec!["owned.txt".to_string()]
        );
    }

    #[tokio::test]
    async fn repo_ci_triage_phase_applies_model_router() {
        let mut base = config::test_config().await;
        base.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                model: Some("triage-model".to_string()),
                service_tier: Some(ServiceTier::Flex),
                ..Default::default()
            }],
            ..Default::default()
        });
        let config = repo_ci_phase_config_from_base(
            base,
            ModelRouterSource::Module("repo_ci.triage"),
            10,
            &[],
        );

        assert_eq!(config.model.as_deref(), Some("triage-model"));
        assert_eq!(config.service_tier, Some(ServiceTier::Flex));
    }

    #[tokio::test]
    async fn repo_ci_review_phase_applies_model_router() {
        let mut base = config::test_config().await;
        base.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                model: Some("review-model".to_string()),
                intelligence_score: Some(0.8),
                ..Default::default()
            }],
            ..Default::default()
        });
        let config = repo_ci_phase_config_from_base(
            base,
            ModelRouterSource::Module("repo_ci.review"),
            10,
            &[],
        );

        assert_eq!(config.model.as_deref(), Some("review-model"));
    }

    #[tokio::test]
    async fn repo_ci_fix_phase_applies_model_router() {
        let mut base = config::test_config().await;
        base.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                model: Some("fix-model".to_string()),
                intelligence_score: Some(0.8),
                ..Default::default()
            }],
            ..Default::default()
        });
        let config =
            repo_ci_phase_config_from_base(base, ModelRouterSource::Module("repo_ci.fix"), 10, &[]);

        assert_eq!(config.model.as_deref(), Some("fix-model"));
    }

    #[test]
    fn remote_classification_never_ignores_whole_suite_failure() {
        let checks = [
            codex_repo_ci::RemoteRepoCiCheck {
                name: "infra outage".to_string(),
                state: "FAILURE".to_string(),
                bucket: Some("fail".to_string()),
                link: None,
            },
            codex_repo_ci::RemoteRepoCiCheck {
                name: "unrelated flaky job".to_string(),
                state: "FAILURE".to_string(),
                bucket: Some("fail".to_string()),
                link: None,
            },
        ];
        let failed = checks.iter().collect::<Vec<_>>();

        assert_eq!(
            classify_remote_failure(checks.len(), &failed),
            FailureClassification::WholeSuite
        );
    }

    #[test]
    fn remote_classification_ignores_only_partial_unrelated_failures() {
        let checks = [
            codex_repo_ci::RemoteRepoCiCheck {
                name: "build".to_string(),
                state: "SUCCESS".to_string(),
                bucket: Some("pass".to_string()),
                link: None,
            },
            codex_repo_ci::RemoteRepoCiCheck {
                name: "infra outage".to_string(),
                state: "FAILURE".to_string(),
                bucket: Some("fail".to_string()),
                link: None,
            },
        ];
        let failed = checks.iter().skip(1).collect::<Vec<_>>();

        assert_eq!(
            classify_remote_failure(checks.len(), &failed),
            FailureClassification::Unrelated
        );
    }

    #[test]
    fn parses_fenced_triage_result() {
        let triage = parse_triage_result(
            r#"```json
{"classification":"related","confidence":"medium","summary":"lint failed","failed_steps":["lint"]}
```"#,
        )
        .expect("triage");

        assert_eq!(
            triage,
            TriageResult {
                classification: FailureClassification::Related,
                confidence: TriageConfidence::Medium,
                summary: "lint failed".to_string(),
                failed_steps: vec!["lint".to_string()],
                model_used: None,
            }
        );
    }

    #[test]
    fn deterministic_unrelated_triage_is_ignorable() {
        let triage = TriageResult::deterministic(
            FailureClassification::Unrelated,
            "failed check names look unrelated",
        );

        assert!(triage.should_ignore());
    }

    #[test]
    fn unknown_triage_is_not_ignorable() {
        let triage = TriageResult::deterministic(
            FailureClassification::Unknown,
            "no model triage result was available",
        );

        assert!(!triage.should_ignore());
    }

    #[test]
    fn learning_requirement_detects_missing_and_stale_manifests() {
        let paths = codex_repo_ci::RepoCiPaths {
            repo_root: Path::new("/tmp/repo").to_path_buf(),
            state_dir: Path::new("/tmp/state").to_path_buf(),
            manifest_path: Path::new("/tmp/state/manifest.json").to_path_buf(),
            runner_path: Path::new("/tmp/state/run_ci.sh").to_path_buf(),
        };
        let missing = codex_repo_ci::StatusOutcome {
            paths: paths.clone(),
            manifest: None,
            stale_sources: Vec::new(),
        };
        assert_eq!(
            repo_ci_learning_requirement(&missing),
            Some(RepoCiLearningRequirement::Initial)
        );

        let stale = codex_repo_ci::StatusOutcome {
            paths,
            manifest: Some(codex_repo_ci::RepoCiManifest {
                version: 3,
                repo_root: Path::new("/tmp/repo").to_path_buf(),
                repo_key: "repo".to_string(),
                source_key: "source".to_string(),
                automation: codex_repo_ci::AutomationMode::Local,
                local_test_time_budget_sec: 300,
                learned_at_unix_sec: 1,
                learning_sources: Vec::new(),
                inferred_issue_types: Vec::new(),
                prepare_steps: Vec::new(),
                fast_steps: Vec::new(),
                full_steps: Vec::new(),
                validation: codex_repo_ci::ValidationStatus::NotRun,
            }),
            stale_sources: vec![codex_repo_ci::SourceHash {
                path: Path::new("Cargo.toml").to_path_buf(),
                sha256: "abc".to_string(),
                kind: codex_repo_ci::SourceKind::BuildManifest,
            }],
        };
        assert_eq!(
            repo_ci_learning_requirement(&stale),
            Some(RepoCiLearningRequirement::Stale(vec![
                Path::new("Cargo.toml").to_path_buf()
            ]))
        );
    }

    #[test]
    fn run_local_repo_ci_requires_prelearned_runner() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo_root = temp.path().join("repo");
        std::fs::create_dir(&repo_root).expect("create repo");

        let err = run_local_repo_ci(
            &codex_home,
            &repo_root,
            &EffectiveRepoCiConfig {
                automation: RepoCiAutomationToml::Local,
                local_test_time_budget_sec: 300,
                long_ci: false,
                max_local_fix_rounds: 3,
                max_remote_fix_rounds: 2,
                review_issue_types: Vec::new(),
                max_review_fix_rounds: 0,
            },
            codex_repo_ci::RepoCiCancellation::default(),
        )
        .expect_err("missing runner should fail");

        assert!(
            err.to_string()
                .contains("run `codex repo-ci learn --cwd` first")
        );
    }
}
