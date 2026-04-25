use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use codex_config::config_toml::RepoCiAutomationToml;
use codex_config::config_toml::RepoCiModelCandidateToml;
use codex_config::config_toml::RepoCiScopeToml;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RepoCiPhase;
use codex_protocol::protocol::RepoCiScope;
use codex_protocol::protocol::RepoCiState;
use codex_protocol::protocol::RepoCiStatusEvent;
use codex_rollout_trace::InferenceTraceContext;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tracing::warn;

use crate::Prompt;
use crate::ResponseEvent;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

const MAX_OUTPUT_BYTES: usize = 24_000;
const TRIAGE_BASE_INSTRUCTIONS: &str =
    "You classify repository CI failures. Return strict JSON only. Do not suggest code edits.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoCiTurnState {
    initial_snapshot: WorktreeSnapshot,
    local_fix_rounds: u8,
    remote_fix_rounds: u8,
    remote_completed: bool,
}

impl RepoCiTurnState {
    pub(crate) fn new(cwd: &Path) -> Self {
        Self {
            initial_snapshot: WorktreeSnapshot::capture(cwd),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveRepoCiConfig {
    automation: RepoCiAutomationToml,
    local_test_time_budget_sec: u64,
    max_local_fix_rounds: u8,
    max_remote_fix_rounds: u8,
    models: Vec<RepoCiModelCandidateToml>,
}

impl EffectiveRepoCiConfig {
    fn from_scope(scope: &RepoCiScopeToml) -> Option<Self> {
        if scope.enabled != Some(true) {
            return None;
        }
        Some(Self {
            automation: scope
                .automation
                .unwrap_or(RepoCiAutomationToml::LocalAndRemote),
            local_test_time_budget_sec: scope.local_test_time_budget_sec.unwrap_or(300),
            max_local_fix_rounds: scope.max_local_fix_rounds.unwrap_or(3),
            max_remote_fix_rounds: scope.max_remote_fix_rounds.unwrap_or(2),
            models: scope.models.clone().unwrap_or_default(),
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
}

pub(crate) async fn maybe_run_after_agent(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    state: &mut RepoCiTurnState,
) -> Option<ResponseItem> {
    let config = effective_config(turn_context)?;
    if !turn_context.config.active_project.is_trusted() {
        return None;
    }
    let current_snapshot = WorktreeSnapshot::capture(&turn_context.cwd);
    if current_snapshot == state.initial_snapshot {
        return None;
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
    let result = tokio::task::spawn_blocking({
        let codex_home = turn_context.config.codex_home.clone();
        let cwd = turn_context.cwd.clone();
        let config = config.clone();
        move || run_local_repo_ci(&codex_home, &cwd, &config)
    })
    .await;
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
            state.initial_snapshot = current_snapshot.clone();
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
                &config,
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
            return Some(repair_prompt(
                "local",
                &output,
                &current_snapshot.changed_paths,
                &current_snapshot.diff_summary,
                &triage,
                &config.models,
            ));
        }
    }

    if !config.remote_enabled() || state.remote_completed {
        return None;
    }

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
        move || run_remote_repo_ci(&cwd)
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
            return None;
        }
    };

    match remote_outcome {
        RemoteRepoCiOutcome::Skipped(reason) => {
            state.remote_completed = true;
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
            None
        }
        RemoteRepoCiOutcome::Passed => {
            state.remote_completed = true;
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
            None
        }
        RemoteRepoCiOutcome::Failed {
            output,
            classification,
        } => {
            let triage = triage_failure(
                sess,
                turn_context,
                &config,
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
            Some(repair_prompt(
                "remote",
                &output,
                &current_snapshot.changed_paths,
                &current_snapshot.diff_summary,
                &triage,
                &config.models,
            ))
        }
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
    let repo_ci = turn_context.config.repo_ci.as_ref()?;
    let cwd = &turn_context.cwd;
    if let Some(scope) = most_specific_directory_scope(cwd, &repo_ci.directories)
        && let Some(config) = EffectiveRepoCiConfig::from_scope(scope)
    {
        return Some(config);
    }
    if let Some(org) = github_org(cwd)
        && let Some(scope) = repo_ci.github_orgs.get(&org)
        && let Some(config) = EffectiveRepoCiConfig::from_scope(scope)
    {
        return Some(config);
    }
    repo_ci
        .defaults
        .as_ref()
        .and_then(EffectiveRepoCiConfig::from_scope)
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
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_org(String::from_utf8_lossy(&output.stdout).trim())
}

fn parse_github_org(remote: &str) -> Option<String> {
    let marker = "github.com";
    let index = remote.find(marker)?;
    let after_host = remote[index + marker.len()..]
        .trim_start_matches(':')
        .trim_start_matches('/');
    after_host.split('/').next().map(str::to_string)
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
) -> Result<LocalRepoCiOutcome> {
    if !config.local_enabled() {
        return Ok(LocalRepoCiOutcome::Skipped);
    }
    let status = codex_repo_ci::status(codex_home, cwd)?;
    if status.manifest.is_none() || !status.stale_sources.is_empty() {
        codex_repo_ci::learn(
            codex_home,
            cwd,
            codex_repo_ci::LearnOptions {
                automation: automation_to_repo_ci(config.automation),
                local_test_time_budget_sec: config.local_test_time_budget_sec,
            },
        )?;
    }
    let run = codex_repo_ci::run_capture(codex_home, cwd, codex_repo_ci::RunMode::Fast)?;
    if run.status.success {
        Ok(LocalRepoCiOutcome::Passed)
    } else {
        Ok(LocalRepoCiOutcome::Failed {
            output: format_run_output("local fast runner", &run.stdout, &run.stderr, &run.steps),
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
    Passed,
    Failed {
        output: String,
        classification: FailureClassification,
    },
}

fn run_remote_repo_ci(cwd: &Path) -> Result<RemoteRepoCiOutcome> {
    if !command_success(
        Command::new("gh")
            .arg("auth")
            .arg("status")
            .current_dir(cwd),
    ) {
        return Ok(RemoteRepoCiOutcome::Skipped(
            "Repo CI remote checks skipped because `gh auth status` failed.".to_string(),
        ));
    }
    let _ = Command::new("gh")
        .args(["auth", "setup-git"])
        .current_dir(cwd)
        .status();

    let Some(pr) = current_pr(cwd)? else {
        return Ok(RemoteRepoCiOutcome::Skipped(
            "Repo CI remote checks skipped because no PR is linked to the current branch."
                .to_string(),
        ));
    };

    push_pr_head(cwd, &pr)?;
    let watch = codex_repo_ci::watch_pr(cwd)?;
    if watch.success() {
        return Ok(RemoteRepoCiOutcome::Passed);
    }

    let checks = pr_checks(cwd)?;
    if checks.is_empty() {
        return Ok(RemoteRepoCiOutcome::Failed {
            output: "GitHub PR checks failed, but `gh pr checks --json` returned no checks."
                .to_string(),
            classification: FailureClassification::Unknown,
        });
    }
    let failed = checks
        .iter()
        .filter(|check| check.bucket.as_deref() == Some("fail") || check.state == "FAILURE")
        .collect::<Vec<_>>();
    if failed.is_empty() {
        return Ok(RemoteRepoCiOutcome::Passed);
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

fn command_success(command: &mut Command) -> bool {
    command.status().is_ok_and(|status| status.success())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    head_ref_name: String,
    head_repository: Option<GhRepository>,
    head_repository_owner: GhRepositoryOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GhRepository {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GhRepositoryOwner {
    login: String,
}

fn current_pr(cwd: &Path) -> Result<Option<GhPr>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            "--json",
            "number,headRefName,headRepository,headRepositoryOwner",
        ])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&output.stdout)?))
}

fn push_pr_head(cwd: &Path, pr: &GhPr) -> Result<()> {
    let mut push_ref = String::from("HEAD:");
    push_ref.push_str(&pr.head_ref_name);
    let remote = if let Some(repo) = &pr.head_repository {
        format!(
            "git@github.com:{}/{}.git",
            pr.head_repository_owner.login, repo.name
        )
    } else {
        "origin".to_string()
    };
    let status = Command::new("git")
        .args(["push", &remote, &push_ref])
        .current_dir(cwd)
        .status()
        .context("failed to run `git push` for PR head")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "`git push {remote} {push_ref}` failed with {status}"
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GhCheck {
    name: String,
    state: String,
    bucket: Option<String>,
    link: Option<String>,
}

fn pr_checks(cwd: &Path) -> Result<Vec<GhCheck>> {
    let output = Command::new("gh")
        .args(["pr", "checks", "--json", "name,state,bucket,link"])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn format_remote_checks(checks: &[GhCheck]) -> String {
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

fn format_run_output(
    label: &str,
    stdout: &str,
    stderr: &str,
    steps: &[codex_repo_ci::CapturedStep],
) -> String {
    let step_output = steps
        .iter()
        .map(|step| format!("{} {:?} {:?}", step.id, step.event, step.exit_code))
        .collect::<Vec<_>>()
        .join("\n");
    truncate_middle(
        &format!("{label}\n\nsteps:\n{step_output}\n\nstdout:\n{stdout}\n\nstderr:\n{stderr}"),
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

fn classify_remote_failure(total_checks: usize, failed: &[&GhCheck]) -> FailureClassification {
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
    config: &EffectiveRepoCiConfig,
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

    for candidate in &config.models {
        match run_model_triage(sess, turn_context, candidate, &input).await {
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
                        "Repo CI triage classified {} failure as {} using {}.",
                        input.kind,
                        triage.classification.as_str(),
                        triage.model_used.as_deref().unwrap_or("configured model")
                    ),
                )
                .await;
                return triage;
            }
            Err(err) => {
                warn!("Repo CI triage model failed: {err:#}");
            }
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
    candidate: &RepoCiModelCandidateToml,
    input: &TriageInput<'_>,
) -> Result<TriageResult> {
    let model = candidate
        .model
        .clone()
        .unwrap_or_else(|| turn_context.model_info.slug.clone());
    let model_info = if candidate.model.is_some() {
        sess.services
            .models_manager
            .get_model_info(&model, &turn_context.config.to_models_manager_config())
            .await
    } else {
        turn_context.model_info.clone()
    };
    let effort = candidate
        .reasoning_effort
        .or(turn_context.reasoning_effort)
        .or(model_info.default_reasoning_level);

    let mut client_session = sess.services.model_client.new_session();
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: triage_prompt_text(input),
            }],
            end_turn: None,
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
            candidate.speed_tier.or(turn_context.config.service_tier),
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
        input.changed_paths.join("\n")
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
    models: &[RepoCiModelCandidateToml],
) -> ResponseItem {
    let model_chain = if models.is_empty() {
        "No repo-ci model chain is configured; use the active model.".to_string()
    } else {
        let models = models
            .iter()
            .map(|candidate| {
                let model = candidate.model.as_deref().unwrap_or("active model");
                let effort = candidate
                    .reasoning_effort
                    .map(|effort| effort.to_string())
                    .unwrap_or_else(|| "default".to_string());
                format!("{model} ({effort})")
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        format!("Configured repo-ci triage model chain: {models}.")
    };
    let changed_paths = if changed_paths.is_empty() {
        "(no changed paths recorded)".to_string()
    } else {
        changed_paths.join("\n")
    };
    let text = format!(
        "Repo CI {kind} checks failed after your changes.\n\n{model_chain}\n\nTriage classification: {} ({:?} confidence)\nTriage summary: {}\n\nChanged paths:\n```text\n{changed_paths}\n```\n\nDiff summary:\n```text\n{}\n```\n\nIf the failure is related or uncertain, fix the code, then let repo CI run again. Do not edit code for clearly unrelated failures.\n\nFailure output:\n```text\n{}\n```",
        triage.classification.as_str(),
        triage.confidence,
        triage.summary,
        truncate_middle(diff_summary, 4_000),
        truncate_middle(output, MAX_OUTPUT_BYTES)
    );
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text }],
        end_turn: None,
        phase: None,
    }
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
    use codex_config::config_toml::RepoCiToml;
    use pretty_assertions::assert_eq;

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
    fn truncate_middle_preserves_utf8_boundaries() {
        let truncated = truncate_middle("prefix 😎 middle 😎 suffix", 12);
        assert!(truncated.contains("omitted"));
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn parses_git_status_changed_paths() {
        assert_eq!(
            parse_status_paths(b" M src/lib.rs\0?? new file.txt\0"),
            vec!["src/lib.rs".to_string(), "new file.txt".to_string()]
        );
    }

    #[test]
    fn remote_classification_never_ignores_whole_suite_failure() {
        let checks = [
            GhCheck {
                name: "infra outage".to_string(),
                state: "FAILURE".to_string(),
                bucket: Some("fail".to_string()),
                link: None,
            },
            GhCheck {
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
            GhCheck {
                name: "build".to_string(),
                state: "SUCCESS".to_string(),
                bucket: Some("pass".to_string()),
                link: None,
            },
            GhCheck {
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
}
