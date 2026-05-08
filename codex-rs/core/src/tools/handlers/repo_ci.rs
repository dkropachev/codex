use crate::config::edit::ConfigEditsBuilder;
use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use codex_cicd_artifacts::RunArtifact;
use codex_cicd_artifacts::RunMode;
use codex_config::CONFIG_TOML_FILE;
use codex_config::config_toml::RepoCiAutomationToml;
use codex_features::Feature;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
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
    instruction: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
struct InstructionArgs {
    action: InstructionAction,
    scope: InstructionScope,
    instruction: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum InstructionAction {
    Show,
    Set,
    Clear,
}

#[derive(Debug, Clone, Deserialize)]
struct InstructionScope {
    #[serde(default)]
    cwd: bool,
    github_repo: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ToolRunMode {
    Prepare,
    #[default]
    Fast,
    Full,
}

impl From<ToolRunMode> for RunMode {
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
        if matches!(invocation.tool_name.name.as_str(), "status" | "result") {
            return false;
        }
        if invocation.tool_name.name.as_str() == "instruction"
            && let ToolPayload::Function { arguments } = &invocation.payload
            && let Ok(args) = serde_json::from_str::<InstructionArgs>(arguments)
        {
            return args.action != InstructionAction::Show;
        }
        true
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
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
                handle_learn(
                    &session,
                    &turn,
                    parse_arguments(&arguments)?,
                    cancellation_token,
                )
                .await
            }
            "run" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_run(
                    &session,
                    &turn,
                    parse_arguments(&arguments)?,
                    cancellation_token,
                )
                .await
            }
            "result" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_result(turn.as_ref(), parse_arguments(&arguments)?).await
            }
            "instruction" => {
                ensure_repo_ci_tool_allowed(turn.as_ref())?;
                handle_instruction(turn.as_ref(), parse_arguments(&arguments)?).await
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
                "learning_instruction".to_string(),
                json!(manifest.learning_instruction),
            );
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
                    "fast_cached_pass": cached_pass_available(turn, RunMode::Fast),
                    "full_cached_pass": cached_pass_available(turn, RunMode::Full),
                }),
            );
        }
    }

    json_output(output)
}

async fn handle_learn(
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    args: LearnArgs,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> Result<String, FunctionCallError> {
    let mut options = learn_options(turn.as_ref(), &args)?;
    if let Some(instruction) = args.instruction.as_deref() {
        let normalized_instruction =
            crate::repo_ci_automation::validate_repo_ci_learning_instruction(
                session,
                turn,
                instruction,
                &cancellation_token,
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "repo_ci.learn rejected instruction: {err:#}"
                ))
            })?;
        if options.learning_instruction.as_deref() != Some(normalized_instruction.as_str()) {
            options.learning_instruction = Some(normalized_instruction.clone());
            persist_repo_ci_learning_instruction(
                &turn.config.codex_home,
                &turn.cwd,
                Some(&normalized_instruction),
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "repo_ci.learn failed to save instruction: {err:#}"
                ))
            })?;
        }
    }
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
        codex_repo_ci::ValidationPhase::Prepare => RunMode::Prepare,
        codex_repo_ci::ValidationPhase::Fast => RunMode::Fast,
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
    session: &Arc<Session>,
    turn: &Arc<TurnContext>,
    args: RunArgs,
    cancellation_token: tokio_util::sync::CancellationToken,
) -> Result<String, FunctionCallError> {
    let mode = RunMode::from(args.mode);
    let status = repo_ci_status(turn.as_ref()).await?;
    let learning_instruction =
        crate::repo_ci_automation::effective_repo_ci_learning_instruction(turn.as_ref());
    let learning_instruction_changed = status
        .manifest
        .as_ref()
        .is_some_and(|manifest| manifest.learning_instruction != learning_instruction);
    if repo_ci_needs_learning(&status, learning_instruction.as_deref()) {
        if !args.learn_if_needed {
            return json_output(json!({
                "status": "needs_learning",
                "mode": mode,
                "learning_instruction_changed": learning_instruction_changed,
                "stale_sources": status.stale_sources.iter().map(source_json).collect::<Vec<_>>(),
            }));
        }
        let learn_args = LearnArgs {
            detail: DetailLevel::Brief,
            automation: None,
            local_test_time_budget_sec: None,
            instruction: None,
        };
        let learn_output =
            handle_learn(session, turn, learn_args, cancellation_token.clone()).await?;
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
        && let Some(artifact) = cached_pass(turn.as_ref(), mode).await?
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
        codex_cicd_artifacts::read_run_artifact(&codex_home, &artifact_id)
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

#[derive(Debug)]
struct ResolvedInstructionScope {
    label: String,
    segments: Vec<String>,
    local_repo_root: Option<std::path::PathBuf>,
}

async fn handle_instruction(
    turn: &crate::session::turn_context::TurnContext,
    args: InstructionArgs,
) -> Result<String, FunctionCallError> {
    let resolved = resolve_instruction_scope(turn, &args.scope)?;
    let old = configured_repo_ci_learning_instruction(&turn.config.codex_home, &resolved.segments)
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "repo_ci.instruction failed to read config: {err:#}"
            ))
        })?;
    match args.action {
        InstructionAction::Show => {
            let configured = old.is_some();
            json_output(json!({
                "scope": resolved.label,
                "instruction": old,
                "configured": configured,
            }))
        }
        InstructionAction::Set => {
            let instruction = args.instruction.as_deref().ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "repo_ci.instruction set requires instruction".to_string(),
                )
            })?;
            let new = normalize_learning_instruction(instruction);
            persist_repo_ci_learning_instruction_for_segments(
                &turn.config.codex_home,
                &resolved.segments,
                new.as_deref(),
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "repo_ci.instruction failed to save config: {err:#}"
                ))
            })?;
            let relearned =
                relearn_after_instruction_change(turn, &resolved, old.as_ref(), new.as_ref())
                    .await?;
            json_output(json!({
                "scope": resolved.label,
                "old_instruction": old,
                "new_instruction": new,
                "relearned": relearned,
            }))
        }
        InstructionAction::Clear => {
            persist_repo_ci_learning_instruction_for_segments(
                &turn.config.codex_home,
                &resolved.segments,
                None,
            )
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "repo_ci.instruction failed to save config: {err:#}"
                ))
            })?;
            let relearned =
                relearn_after_instruction_change(turn, &resolved, old.as_ref(), None).await?;
            json_output(json!({
                "scope": resolved.label,
                "old_instruction": old,
                "new_instruction": null,
                "relearned": relearned,
            }))
        }
    }
}

fn resolve_instruction_scope(
    turn: &crate::session::turn_context::TurnContext,
    scope: &InstructionScope,
) -> Result<ResolvedInstructionScope, FunctionCallError> {
    let specified = (scope.cwd as usize) + (scope.github_repo.is_some() as usize);
    if specified != 1 {
        return Err(FunctionCallError::RespondToModel(
            "repo_ci.instruction scope requires exactly one of cwd or github_repo".to_string(),
        ));
    }
    if scope.cwd {
        let repo_root = codex_repo_ci::repo_root_for_cwd(&turn.cwd).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "repo_ci.instruction failed to resolve cwd repo: {err:#}"
            ))
        })?;
        let label = instruction_scope_label(&repo_root);
        return Ok(ResolvedInstructionScope {
            label,
            segments: repo_ci_current_repo_scope_segments(&repo_root),
            local_repo_root: Some(repo_root),
        });
    }
    let Some(repo) = scope.github_repo.as_ref() else {
        return Err(FunctionCallError::RespondToModel(
            "repo_ci.instruction scope requires exactly one of cwd or github_repo".to_string(),
        ));
    };
    validate_github_repo_scope(repo)?;
    Ok(ResolvedInstructionScope {
        label: format!("github_repo:{repo}"),
        segments: vec![
            "repo_ci".to_string(),
            "github_repos".to_string(),
            repo.clone(),
        ],
        local_repo_root: current_repo_root_if_github_repo(&turn.cwd, repo),
    })
}

fn validate_github_repo_scope(repo: &str) -> Result<(), FunctionCallError> {
    let mut parts = repo.split('/');
    let valid = parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_none();
    if !valid {
        return Err(FunctionCallError::RespondToModel(
            "repo_ci.instruction github_repo scope must be `org/repo`".to_string(),
        ));
    }
    Ok(())
}

fn current_repo_root_if_github_repo(cwd: &Path, repo: &str) -> Option<std::path::PathBuf> {
    let repo_root = codex_repo_ci::repo_root_for_cwd(cwd).ok()?;
    (codex_repo_ci::github_repo_slug(&repo_root).as_deref() == Some(repo)).then_some(repo_root)
}

fn instruction_scope_label(repo_root: &Path) -> String {
    codex_repo_ci::github_repo_slug(repo_root)
        .map(|repo| format!("github_repo:{repo}"))
        .unwrap_or_else(|| format!("directory:{}", repo_root.display()))
}

async fn relearn_after_instruction_change(
    turn: &crate::session::turn_context::TurnContext,
    resolved: &ResolvedInstructionScope,
    old: Option<&String>,
    new: Option<&String>,
) -> Result<bool, FunctionCallError> {
    if old == new {
        return Ok(false);
    }
    let Some(repo_root) = resolved.local_repo_root.clone() else {
        return Ok(false);
    };
    let codex_home = turn.config.codex_home.clone();
    let status = tokio::task::spawn_blocking({
        let codex_home = codex_home.clone();
        let repo_root = repo_root.clone();
        move || codex_repo_ci::status(&codex_home, &repo_root)
    })
    .await
    .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err}")))?
    .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.status failed: {err:#}")))?;
    let automation = status
        .manifest
        .as_ref()
        .map(|manifest| manifest.automation)
        .unwrap_or(codex_repo_ci::AutomationMode::LocalAndRemote);
    let local_test_time_budget_sec = status
        .manifest
        .as_ref()
        .map(|manifest| manifest.local_test_time_budget_sec)
        .unwrap_or(DEFAULT_LOCAL_TEST_TIME_BUDGET_SEC);
    let options = codex_repo_ci::LearnOptions {
        automation,
        local_test_time_budget_sec,
        learning_instruction: new.cloned(),
    };
    tokio::task::spawn_blocking(move || codex_repo_ci::learn(&codex_home, &repo_root, options))
        .await
        .map_err(|err| FunctionCallError::RespondToModel(format!("repo_ci.learn failed: {err}")))?
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("repo_ci.learn failed: {err:#}"))
        })?;
    Ok(true)
}

fn configured_repo_ci_learning_instruction(
    codex_home: &Path,
    segments: &[String],
) -> anyhow::Result<Option<String>> {
    let singular_segments = append_segment(segments, "learning_instruction");
    if let Some(item) = repo_ci_config_item(codex_home, &singular_segments)?
        && let Some(instruction) = repo_ci_learning_instruction_from_item(&item)
    {
        return Ok(Some(instruction));
    }
    let legacy_segments = append_segment(segments, "learning_instructions");
    let Some(item) = repo_ci_config_item(codex_home, &legacy_segments)? else {
        return Ok(None);
    };
    Ok(repo_ci_learning_instruction_from_item(&item))
}

fn repo_ci_config_item(
    codex_home: &Path,
    segments: &[String],
) -> anyhow::Result<Option<toml_edit::Item>> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    if !config_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(config_path)?;
    let doc = raw.parse::<toml_edit::DocumentMut>()?;
    let mut item = doc.as_item();
    for segment in segments {
        let Some(next) = item.get(segment) else {
            return Ok(None);
        };
        item = next;
    }
    Ok(Some(item.clone()))
}

fn repo_ci_learning_instruction_from_item(item: &toml_edit::Item) -> Option<String> {
    if let Some(value) = item.as_str() {
        return normalize_learning_instruction(value);
    }
    item.as_array().and_then(|array| {
        normalize_learning_instruction(
            &array
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        )
    })
}

fn normalize_learning_instruction(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
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
    mode: RunMode,
) -> Result<Option<RunArtifact>, FunctionCallError> {
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

fn cached_pass_available(turn: &crate::session::turn_context::TurnContext, mode: RunMode) -> bool {
    let codex_home = turn.config.codex_home.clone();
    let cwd = turn.cwd.clone();
    codex_repo_ci::lookup_cached_passing_run(&codex_home, &cwd, mode)
        .ok()
        .flatten()
        .is_some()
}

fn repo_ci_needs_learning(
    status: &codex_repo_ci::StatusOutcome,
    learning_instruction: Option<&str>,
) -> bool {
    status
        .manifest
        .as_ref()
        .is_none_or(|manifest| manifest.learning_instruction.as_deref() != learning_instruction)
        || !status.stale_sources.is_empty()
}

fn learn_options(
    turn: &TurnContext,
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
        learning_instruction: crate::repo_ci_automation::effective_repo_ci_learning_instruction(
            turn,
        ),
    })
}

async fn persist_repo_ci_learning_instruction(
    codex_home: &Path,
    cwd: &Path,
    instruction: Option<&str>,
) -> anyhow::Result<()> {
    let repo_root = codex_repo_ci::repo_root_for_cwd(cwd)?;
    persist_repo_ci_learning_instruction_for_segments(
        codex_home,
        &repo_ci_current_repo_scope_segments(&repo_root),
        instruction,
    )
    .await
}

async fn persist_repo_ci_learning_instruction_for_segments(
    codex_home: &Path,
    segments: &[String],
    instruction: Option<&str>,
) -> anyhow::Result<()> {
    let instruction = instruction.and_then(normalize_learning_instruction);
    let builder = ConfigEditsBuilder::new(codex_home)
        .clear_path(append_segment(segments, "learning_instructions"));
    let builder = if let Some(instruction) = instruction {
        builder.set_path_value(
            append_segment(segments, "learning_instruction"),
            toml_edit::value(instruction),
        )
    } else {
        builder.clear_path(append_segment(segments, "learning_instruction"))
    };
    builder.apply().await
}

fn repo_ci_current_repo_scope_segments(repo_root: &Path) -> Vec<String> {
    if let Some(repo) = codex_repo_ci::github_repo_slug(repo_root) {
        return vec!["repo_ci".to_string(), "github_repos".to_string(), repo];
    }
    vec![
        "repo_ci".to_string(),
        "directories".to_string(),
        repo_root.to_string_lossy().to_string(),
    ]
}

fn append_segment(segments: &[String], segment: &str) -> Vec<String> {
    let mut updated = segments.to_vec();
    updated.push(segment.to_string());
    updated
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
