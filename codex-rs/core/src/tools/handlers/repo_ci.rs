use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_config::config_toml::RepoCiAutomationToml;
use codex_features::Feature;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[path = "repo_ci_output.rs"]
mod repo_ci_output;

use repo_ci_output::DETAILED_LOG_MAX_BYTES;
use repo_ci_output::artifact_metadata_json;
use repo_ci_output::bounded_log;
use repo_ci_output::format_run_artifact_response;
use repo_ci_output::json_output;
use repo_ci_output::select_step_output;
use repo_ci_output::source_json;

const DEFAULT_LOCAL_TEST_TIME_BUDGET_SEC: u64 = 300;

pub struct RepoCiHandler;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DetailLevel {
    #[default]
    Brief,
    Detailed,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReuseMode {
    #[default]
    Auto,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
struct StatusArgs {
    #[serde(default)]
    detail: DetailLevel,
}

#[derive(Debug, Clone, Deserialize)]
struct LearnArgs {
    #[serde(default)]
    detail: DetailLevel,
    automation: Option<String>,
    local_test_time_budget_sec: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunArgs {
    #[serde(default = "default_run_mode")]
    mode: ToolRunMode,
    #[serde(default)]
    detail: DetailLevel,
    #[serde(default)]
    reuse: ReuseMode,
    #[serde(default = "default_true")]
    learn_if_needed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ResultArgs {
    artifact_id: String,
    #[serde(default)]
    detail: DetailLevel,
    step_id: Option<String>,
    tail_lines: Option<usize>,
    max_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ToolRunMode {
    Prepare,
    #[default]
    Fast,
    Full,
}

impl From<ToolRunMode> for codex_repo_ci::RunMode {
    fn from(value: ToolRunMode) -> Self {
        match value {
            ToolRunMode::Prepare => Self::Prepare,
            ToolRunMode::Fast => Self::Fast,
            ToolRunMode::Full => Self::Full,
        }
    }
}

impl ToolHandler for RepoCiHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, invocation: &ToolInvocation) -> bool {
        !matches!(invocation.tool_name.name.as_str(), "status" | "result")
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session: _,
            turn,
            cancellation_token,
            tool_name,
            payload,
            ..
        } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "repo_ci tools expect JSON function arguments".to_string(),
                ));
            }
        };

        let output = match tool_name.name.as_str() {
            "status" => handle_status(turn.as_ref(), parse_arguments(&arguments)?).await,
            "learn" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_learn(turn.as_ref(), parse_arguments(&arguments)?).await
            }
            "run" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_run(
                    turn.as_ref(),
                    parse_arguments(&arguments)?,
                    cancellation_token,
                )
                .await
            }
            "result" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_result(turn.as_ref(), parse_arguments(&arguments)?).await
            }
            name => Err(FunctionCallError::RespondToModel(format!(
                "unknown repo_ci tool `{name}`"
            ))),
        }?;
        Ok(FunctionToolOutput::from_text(output, Some(true)))
    }
}

async fn handle_status(
    turn: &crate::session::turn_context::TurnContext,
    args: StatusArgs,
) -> Result<String, FunctionCallError> {
    let trusted = turn.config.active_project.is_trusted();
    if !trusted {
        return json_output(json!({
            "trusted": false,
            "repo_root": turn.cwd,
            "learned": false,
            "stale": false,
            "stale_sources": [],
            "available_modes": [],
            "last_validation": null,
        }));
    }

    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    let status = tokio::task::spawn_blocking(move || codex_repo_ci::status(&codex_home, &cwd))
        .await
        .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err}")))?
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err:#}"))
        })?;
    let learned = status.manifest.is_some();
    let stale_sources = status
        .stale_sources
        .iter()
        .map(source_json)
        .collect::<Vec<_>>();
    let last_validation = status
        .manifest
        .as_ref()
        .map(|manifest| json!(manifest.validation));
    let mut output = json!({
        "trusted": true,
        "repo_root": status.paths.repo_root,
        "learned": learned,
        "stale": !status.stale_sources.is_empty(),
        "stale_sources": stale_sources,
        "available_modes": if learned { vec!["prepare", "fast", "full"] } else { Vec::<&str>::new() },
        "last_validation": last_validation,
    });

    if args.detail == DetailLevel::Detailed
        && let Some(object) = output.as_object_mut()
    {
        object.insert(
            "manifest_path".to_string(),
            json!(status.paths.manifest_path),
        );
        object.insert("runner_path".to_string(), json!(status.paths.runner_path));
        object.insert("state_dir".to_string(), json!(status.paths.state_dir));
        if let Some(manifest) = status.manifest.as_ref() {
            object.insert(
                "learning_source_hashes".to_string(),
                json!(
                    manifest
                        .learning_sources
                        .iter()
                        .map(source_json)
                        .collect::<Vec<_>>()
                ),
            );
            object.insert("automation".to_string(), json!(manifest.automation));
            object.insert(
                "manifest_fingerprint".to_string(),
                json!(codex_repo_ci::manifest_fingerprint(manifest)),
            );
            object.insert(
                "registered_steps".to_string(),
                json!({
                    "prepare": &manifest.prepare_steps,
                    "fast": &manifest.fast_steps,
                    "full": &manifest.full_steps,
                }),
            );
            object.insert(
                "cache_summary".to_string(),
                json!({
                    "fast_cached_pass": cached_pass_available(turn, codex_repo_ci::RunMode::Fast),
                    "full_cached_pass": cached_pass_available(turn, codex_repo_ci::RunMode::Full),
                }),
            );
        }
    }

    json_output(output)
}

async fn handle_learn(
    turn: &crate::session::turn_context::TurnContext,
    args: LearnArgs,
) -> Result<String, FunctionCallError> {
    let options = learn_options(turn, &args)?;
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    let outcome =
        tokio::task::spawn_blocking(move || codex_repo_ci::learn(&codex_home, &cwd, options))
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("repo_ci.learn failed: {err}"))
            })?
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!("repo_ci.learn failed: {err:#}"))
            })?;
    let mode = match outcome.validation_phase {
        codex_repo_ci::ValidationPhase::Prepare => codex_repo_ci::RunMode::Prepare,
        codex_repo_ci::ValidationPhase::Fast => codex_repo_ci::RunMode::Fast,
    };
    let artifact = codex_repo_ci::store_captured_run_artifact(
        &turn.config.codex_home,
        &outcome.paths,
        &outcome.manifest,
        mode,
        &outcome.validation_run,
        Duration::ZERO,
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "repo_ci.learn failed to store artifact: {err:#}"
        ))
    })?;

    json_output(format_run_artifact_response(
        "learn",
        &artifact,
        args.detail,
        /*cache_hit*/ false,
    ))
}

async fn handle_run(
    turn: &crate::session::turn_context::TurnContext,
    args: RunArgs,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> Result<String, FunctionCallError> {
    let mode = codex_repo_ci::RunMode::from(args.mode);
    let status = repo_ci_status(turn).await?;
    if repo_ci_needs_learning(&status) {
        if !args.learn_if_needed {
            return json_output(json!({
                "status": "needs_learning",
                "mode": mode,
                "stale_sources": status.stale_sources.iter().map(source_json).collect::<Vec<_>>(),
            }));
        }
        let learn_args = LearnArgs {
            detail: DetailLevel::Brief,
            automation: None,
            local_test_time_budget_sec: None,
        };
        let learn_output = handle_learn(turn, learn_args).await?;
        let learn_value =
            serde_json::from_str::<serde_json::Value>(&learn_output).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "repo_ci.run failed to parse learn result: {err}"
                ))
            })?;
        if learn_value
            .get("status")
            .and_then(serde_json::Value::as_str)
            != Some("passed")
        {
            return json_output(json!({
                "status": "learn_failed",
                "mode": mode,
                "learn_result": learn_value,
            }));
        }
    }

    if args.reuse == ReuseMode::Auto
        && let Some(artifact) = cached_pass(turn, mode).await?
    {
        return json_output(format_run_artifact_response(
            "run",
            &artifact,
            args.detail,
            /*cache_hit*/ true,
        ));
    }

    let repo_ci_cancellation = codex_repo_ci::RepoCiCancellation::default();
    let cancellation_task = tokio::spawn({
        let cancellation_token = cancellation_token.clone();
        let repo_ci_cancellation = repo_ci_cancellation.clone();
        async move {
            cancellation_token.cancelled().await;
            repo_ci_cancellation.cancel();
        }
    });
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    let result = tokio::task::spawn_blocking(move || {
        codex_repo_ci::run_capture_persisted_with_cancellation(
            &codex_home,
            &cwd,
            mode,
            repo_ci_cancellation,
        )
    })
    .await;
    cancellation_task.abort();
    let artifact = result
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("repo_ci.run task failed: {err}"))
        })?
        .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.run failed: {err:#}")))?;

    json_output(format_run_artifact_response(
        "run",
        &artifact,
        args.detail,
        /*cache_hit*/ false,
    ))
}

async fn handle_result(
    turn: &crate::session::turn_context::TurnContext,
    args: ResultArgs,
) -> Result<String, FunctionCallError> {
    let codex_home = turn.config.codex_home.clone();
    let artifact_id = args.artifact_id.clone();
    let artifact = tokio::task::spawn_blocking(move || {
        codex_repo_ci::read_run_artifact(&codex_home, &artifact_id)
    })
    .await
    .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.result failed: {err}")))?
    .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.result failed: {err:#}")))?;

    let mut output = artifact_metadata_json(&artifact);
    if args.detail == DetailLevel::Detailed
        && let Some(object) = output.as_object_mut()
    {
        let max_bytes = args.max_bytes.unwrap_or(DETAILED_LOG_MAX_BYTES);
        let stdout = select_step_output(&artifact.stdout, args.step_id.as_deref());
        let stderr = select_step_output(&artifact.stderr, args.step_id.as_deref());
        object.insert(
            "stdout".to_string(),
            json!(bounded_log(&stdout, args.tail_lines, max_bytes)),
        );
        object.insert(
            "stderr".to_string(),
            json!(bounded_log(&stderr, args.tail_lines, max_bytes)),
        );
        object.insert("step_id".to_string(), json!(args.step_id));
    }
    json_output(output)
}

fn ensure_repo_ci_tool_allowed(
    turn: &crate::session::turn_context::TurnContext,
) -> Result<(), FunctionCallError> {
    if !turn.config.features.enabled(Feature::RepoCi) {
        return Err(FunctionCallError::RespondToModel(
            "repo-ci tools are unavailable because the repo_ci feature is disabled".to_string(),
        ));
    }
    if !turn.config.active_project.is_trusted() {
        return Err(FunctionCallError::RespondToModel(
            "repo-ci tools are unavailable for this project because it is not trusted".to_string(),
        ));
    }
    if !turn.tools_config.has_environment {
        return Err(FunctionCallError::RespondToModel(
            "repo-ci tools are unavailable because this session has no environment".to_string(),
        ));
    }
    Ok(())
}

async fn repo_ci_status(
    turn: &crate::session::turn_context::TurnContext,
) -> Result<codex_repo_ci::StatusOutcome, FunctionCallError> {
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    tokio::task::spawn_blocking(move || codex_repo_ci::status(&codex_home, &cwd))
        .await
        .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err}")))?
        .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err:#}")))
}

async fn cached_pass(
    turn: &crate::session::turn_context::TurnContext,
    mode: codex_repo_ci::RunMode,
) -> Result<Option<codex_repo_ci::RepoCiRunArtifact>, FunctionCallError> {
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    tokio::task::spawn_blocking(move || {
        codex_repo_ci::lookup_cached_passing_run(&codex_home, &cwd, mode)
    })
    .await
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("repo_ci cache lookup failed: {err}"))
    })?
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!("repo_ci cache lookup failed: {err:#}"))
    })
}

fn cached_pass_available(
    turn: &crate::session::turn_context::TurnContext,
    mode: codex_repo_ci::RunMode,
) -> bool {
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    codex_repo_ci::lookup_cached_passing_run(&codex_home, &cwd, mode)
        .ok()
        .flatten()
        .is_some()
}

fn repo_ci_needs_learning(status: &codex_repo_ci::StatusOutcome) -> bool {
    status.manifest.is_none() || !status.stale_sources.is_empty()
}

fn learn_options(
    turn: &crate::session::turn_context::TurnContext,
    args: &LearnArgs,
) -> Result<codex_repo_ci::LearnOptions, FunctionCallError> {
    let automation = args
        .automation
        .as_deref()
        .map(parse_automation)
        .transpose()?
        .unwrap_or_else(|| {
            turn.config
                .repo_ci
                .as_ref()
                .and_then(|repo_ci| repo_ci.defaults.as_ref())
                .and_then(|defaults| defaults.automation)
                .unwrap_or(RepoCiAutomationToml::Local)
        });
    Ok(codex_repo_ci::LearnOptions {
        automation: match automation {
            RepoCiAutomationToml::Local => codex_repo_ci::AutomationMode::Local,
            RepoCiAutomationToml::Remote => codex_repo_ci::AutomationMode::Remote,
            RepoCiAutomationToml::LocalAndRemote => codex_repo_ci::AutomationMode::LocalAndRemote,
        },
        local_test_time_budget_sec: args.local_test_time_budget_sec.unwrap_or_else(|| {
            turn.config
                .repo_ci
                .as_ref()
                .and_then(|repo_ci| repo_ci.defaults.as_ref())
                .and_then(|defaults| defaults.local_test_time_budget_sec)
                .unwrap_or(DEFAULT_LOCAL_TEST_TIME_BUDGET_SEC)
        }),
    })
}

fn parse_automation(value: &str) -> Result<RepoCiAutomationToml, FunctionCallError> {
    match value {
        "local" => Ok(RepoCiAutomationToml::Local),
        "remote" => Ok(RepoCiAutomationToml::Remote),
        "local-and-remote" => Ok(RepoCiAutomationToml::LocalAndRemote),
        other => Err(FunctionCallError::RespondToModel(format!(
            "invalid repo_ci automation `{other}`; expected local, remote, or local-and-remote"
        ))),
    }
}

fn default_true() -> bool {
    true
}

fn default_run_mode() -> ToolRunMode {
    ToolRunMode::Fast
}

#[cfg(test)]
#[path = "repo_ci_tests.rs"]
mod tests;
