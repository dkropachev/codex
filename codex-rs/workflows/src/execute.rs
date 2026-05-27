use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use codex_config::CONFIG_TOML_FILE;
use codex_config::types::WorkflowsConfigToml;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use toml_edit::Array;
use toml_edit::DocumentMut;
use toml_edit::Item;
use toml_edit::Table;
use toml_edit::value;

use crate::command::WorkflowCommand;
use crate::command::WorkflowConfigCommand;
use crate::command::WorkflowInputSource;
use crate::id::normalize_workflow_id;
#[cfg(test)]
use crate::quality_hook::workflow_quality_block_reason_for_path;
use crate::quality_hook::workflow_quality_block_reason_for_workflow;
use crate::registry::DEFAULT_MAX_REPAIR_CYCLES;
use crate::registry::WorkflowRoot;
use crate::registry::WorkflowSummary;
use crate::registry::default_workflow_root;
use crate::registry::discover_workflows;
use crate::registry::find_workflow;
use crate::registry::summarize_workflow;
#[cfg(test)]
use crate::registry::validate_workflow_dir;
use crate::registry::workflow_impact;
use crate::registry::workflow_roots;
use crate::repair::repair_workflow_command;
use crate::runtime_progress::standalone_cli_runtime_event_handler;
use crate::spec::WORKFLOW_YAML;
use crate::spec::read_workflow_spec;
use crate::spec::scaffold_workflow_spec;
use crate::spec::write_workflow_spec;
use crate::staging::StageRootGuard;
use crate::staging::copy_dir_recursive;
use crate::staging::create_session_stage_root;
use crate::staging::create_stage_root;
use crate::staging::publish_staged_workflow;
use crate::staging::session_stage_root_path;
use crate::validation_runner::run_validation_command;
#[cfg(test)]
use crate::validation_runner::validate_workflow;
use crate::validation_runner::validation_report_message;
use crate::workflow_api::validate_and_publish_workflow_api;
use crate::workflow_api::validate_workflow_api_contract;
use crate::workflow_runtime;

#[derive(Clone)]
pub struct WorkflowCommandContext<'a> {
    pub codex_home: &'a Path,
    pub cwd: &'a Path,
    pub config: &'a WorkflowsConfigToml,
    pub codex_self_exe: Option<PathBuf>,
    pub stage_session_id: Option<String>,
    pub progress: Option<&'a WorkflowCommandProgressHandler<'a>>,
}

pub type WorkflowCommandProgressHandler<'a> = dyn Fn(WorkflowCommandProgress) + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandProgress {
    pub message: String,
    pub data: Option<JsonValue>,
}

impl WorkflowCommandContext<'_> {
    pub(crate) fn report_progress(&self, message: impl Into<String>, data: JsonValue) {
        if let Some(progress) = self.progress {
            progress(WorkflowCommandProgress {
                message: message.into(),
                data: Some(data),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandOutput {
    pub message: String,
    pub data: JsonValue,
}

struct StagedWorkflow {
    _guard: Option<StageRootGuard>,
    root: WorkflowRoot,
    path: PathBuf,
    live_path: PathBuf,
}

pub fn execute_workflow_command(
    ctx: WorkflowCommandContext<'_>,
    command: WorkflowCommand,
) -> Result<WorkflowCommandOutput> {
    match tokio::runtime::Handle::try_current() {
        Ok(_) => std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(execute_workflow_command_async(ctx, command))
            });
            handle
                .join()
                .map_err(|panic| anyhow!("workflow command helper thread panicked: {panic:?}"))?
        }),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(execute_workflow_command_async(ctx, command)),
    }
}

async fn execute_workflow_command_async(
    ctx: WorkflowCommandContext<'_>,
    command: WorkflowCommand,
) -> Result<WorkflowCommandOutput> {
    match command {
        WorkflowCommand::Mode => show_mode(ctx),
        WorkflowCommand::Develop { description } => develop(ctx, &description),
        WorkflowCommand::Describe { id, description } => describe(ctx, &id, &description),
        WorkflowCommand::Docs { id, instruction } => docs(ctx, &id, &instruction),
        WorkflowCommand::Edit { id, instruction } => edit(ctx, &id, &instruction),
        WorkflowCommand::Fix { id } => fix(ctx, &id),
        WorkflowCommand::Run {
            id,
            input,
            input_fields,
        } => run(ctx, &id, input, input_fields).await,
        WorkflowCommand::Validate { id } => validate(ctx, &id),
        WorkflowCommand::Impact { id } => impact(ctx, &id),
        WorkflowCommand::Status { id } => status(ctx, id.as_deref()),
        WorkflowCommand::List => list(ctx),
        WorkflowCommand::Show { id } => show(ctx, &id),
        WorkflowCommand::Where { id } => where_workflow(ctx, &id),
        WorkflowCommand::Config(config_command) => config(ctx, config_command),
        WorkflowCommand::Publish => publish(ctx),
        WorkflowCommand::Discard => discard(ctx),
        WorkflowCommand::Done => {
            if let Some(session_id) = ctx.stage_session_id.as_deref() {
                publish_session_staged_workflows(&ctx, session_id)?;
            }
            Ok(WorkflowCommandOutput {
                message: "Workflow Mode is done.".to_string(),
                data: json!({ "done": true }),
            })
        }
    }
}

fn publish(ctx: WorkflowCommandContext<'_>) -> Result<WorkflowCommandOutput> {
    let session_id = ctx
        .stage_session_id
        .as_deref()
        .ok_or_else(|| anyhow!("workflow publish requires a stage session id"))?;
    let published_workflows = discover_session_staged_workflows(&ctx, session_id)?;
    publish_session_staged_workflows(&ctx, session_id)?;
    Ok(WorkflowCommandOutput {
        message: if published_workflows.is_empty() {
            "No staged workflow changes to publish.".to_string()
        } else {
            format!(
                "Published {} staged workflow(s).",
                published_workflows.len()
            )
        },
        data: json!({
            "published": true,
            "workflowCount": published_workflows.len(),
            "workflows": published_workflows,
        }),
    })
}

fn discard(ctx: WorkflowCommandContext<'_>) -> Result<WorkflowCommandOutput> {
    let session_id = ctx
        .stage_session_id
        .as_deref()
        .ok_or_else(|| anyhow!("workflow discard requires a stage session id"))?;
    let discarded_workflows = discover_session_staged_workflows(&ctx, session_id)?;
    discard_session_staged_workflows(&ctx, session_id)?;
    Ok(WorkflowCommandOutput {
        message: if discarded_workflows.is_empty() {
            "No staged workflow changes to discard.".to_string()
        } else {
            format!(
                "Discarded {} staged workflow(s).",
                discarded_workflows.len()
            )
        },
        data: json!({
            "discarded": true,
            "workflowCount": discarded_workflows.len(),
            "workflows": discarded_workflows,
        }),
    })
}

fn show_mode(ctx: WorkflowCommandContext<'_>) -> Result<WorkflowCommandOutput> {
    let workflows = discover_workflows_for_context(&ctx)?;
    Ok(WorkflowCommandOutput {
        message: format!(
            "Workflow Mode ready. {} workflow(s) discovered. Use `codex workflow list` or `/workflow list`.",
            workflows.len()
        ),
        data: json!({
            "workflowCount": workflows.len(),
            "defaults": effective_config(ctx.config),
        }),
    })
}

fn list(ctx: WorkflowCommandContext<'_>) -> Result<WorkflowCommandOutput> {
    let workflows = discover_workflows_for_context(&ctx)?;
    let message = if workflows.is_empty() {
        "No workflows found.".to_string()
    } else {
        workflows
            .iter()
            .map(|workflow| {
                let title = workflow.title.as_deref().unwrap_or("untitled");
                format!("{}\t{}\t{}", workflow.id, title, workflow.root_label)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(WorkflowCommandOutput {
        message,
        data: json!({ "workflows": workflows }),
    })
}

fn show(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let spec = read_workflow_spec(&workflow.workflow_yaml_path)?;
    Ok(WorkflowCommandOutput {
        message: serde_yaml::to_string(&spec)?,
        data: json!({ "workflow": workflow, "spec": spec }),
    })
}

fn where_workflow(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    Ok(WorkflowCommandOutput {
        message: workflow.path.display().to_string(),
        data: json!({ "workflow": workflow }),
    })
}

fn validate(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let report = if ctx.stage_session_id.is_some() {
        validate_workflow_api_contract(&workflow, run_validation_command)?
    } else {
        validate_and_publish_workflow_api(
            ctx.codex_home,
            ctx.cwd,
            ctx.config,
            &workflow,
            run_validation_command,
        )?
    };
    Ok(WorkflowCommandOutput {
        message: validation_report_message(&report),
        data: json!({ "workflow": workflow, "validation": report }),
    })
}

fn impact(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let impact = workflow_impact(&workflow)?;
    Ok(WorkflowCommandOutput {
        message: serde_json::to_string_pretty(&impact)?,
        data: json!({ "impact": impact }),
    })
}

fn status(ctx: WorkflowCommandContext<'_>, id: Option<&str>) -> Result<WorkflowCommandOutput> {
    if let Some(id) = id {
        let workflow = resolve_workflow_for_context(&ctx, id)?;
        let impact = workflow_impact(&workflow)?;
        let message = if impact.git_status.is_empty() {
            format!("{} is clean", workflow.id)
        } else {
            impact.git_status.join("\n")
        };
        return Ok(WorkflowCommandOutput {
            message,
            data: json!({ "workflow": workflow, "impact": impact }),
        });
    }

    let workflows = discover_workflows_for_context(&ctx)?;
    Ok(WorkflowCommandOutput {
        message: format!("{} workflow(s) discovered", workflows.len()),
        data: json!({ "workflows": workflows, "defaults": effective_config(ctx.config) }),
    })
}

fn develop(ctx: WorkflowCommandContext<'_>, description: &str) -> Result<WorkflowCommandOutput> {
    let live_root = default_workflow_root(ctx.codex_home, ctx.cwd, ctx.config);
    fs::create_dir_all(&live_root.path).with_context(|| {
        format!(
            "failed to create workflow root {}",
            live_root.path.display()
        )
    })?;
    let stage_root_path = match ctx.stage_session_id.as_deref() {
        Some(session_id) => create_session_stage_root(&live_root.path, session_id)?,
        None => create_stage_root(&live_root.path)?,
    };
    let stage_root = WorkflowRoot {
        kind: live_root.kind,
        label: live_root.label.clone(),
        path: stage_root_path.clone(),
    };
    let slug_roots = if ctx.stage_session_id.is_some() {
        vec![live_root.path.as_path(), stage_root.path.as_path()]
    } else {
        vec![live_root.path.as_path()]
    };
    let slug = unique_slug_in_roots(&slug_roots, &slugify(description))?;
    let id = normalize_workflow_id(&slug)?;
    let path = stage_root.path.join(&id);
    fs::create_dir_all(path.join("src"))?;
    fs::create_dir_all(path.join("src/tests"))?;
    fs::create_dir_all(path.join("state"))?;

    let title = title_from_description(description);
    let spec = scaffold_workflow_spec(
        id.clone(),
        title.clone(),
        description.to_string(),
        ctx.config,
    );
    write_workflow_spec(&path.join(WORKFLOW_YAML), &spec)?;
    write_scaffold_files(&path, &id, &title, description)?;
    let live_path = live_root.path.join(&id);
    let staged = StagedWorkflow {
        _guard: ctx
            .stage_session_id
            .is_none()
            .then(|| StageRootGuard::new(stage_root_path)),
        root: stage_root,
        path,
        live_path,
    };
    let had_changes = finalize_staged_workflow_changes(&ctx, &staged, "Create workflow scaffold")?;
    if had_changes && ctx.stage_session_id.is_none() {
        publish_staged_workflow(&staged.root.path, &staged.path, &staged.live_path)?;
    }
    let workflow = resolve_workflow_for_context(&ctx, &id)?;
    let workflow_id = workflow.id.clone();
    let workflow_path = workflow.path.clone();

    Ok(WorkflowCommandOutput {
        message: format!("Created workflow {id} at {}", workflow_path.display()),
        data: json!({ "id": workflow_id, "path": workflow_path, "workflow": workflow }),
    })
}

fn describe(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    description: &str,
) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let staged = stage_existing_workflow(&ctx, &workflow)?;
    let mut spec = read_workflow_spec(&workflow.workflow_yaml_path)?;
    spec.user_description = Some(description.to_string());
    write_workflow_spec(&staged.path.join(WORKFLOW_YAML), &spec)?;
    let had_changes =
        finalize_staged_workflow_changes(&ctx, &staged, "Update workflow description")?;
    if had_changes && ctx.stage_session_id.is_none() {
        publish_staged_workflow(&staged.root.path, &staged.path, &staged.live_path)?;
    }
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    Ok(WorkflowCommandOutput {
        message: format!("Updated description for {}", workflow.id),
        data: json!({ "workflow": workflow, "spec": spec }),
    })
}

fn docs(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    instruction: &str,
) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let staged = stage_existing_workflow(&ctx, &workflow)?;
    append_readme_note(&staged.path, "Documentation", instruction)?;
    let had_changes =
        finalize_staged_workflow_changes(&ctx, &staged, "Update workflow documentation")?;
    if had_changes && ctx.stage_session_id.is_none() {
        publish_staged_workflow(&staged.root.path, &staged.path, &staged.live_path)?;
    }
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    Ok(WorkflowCommandOutput {
        message: format!("Updated docs for {}", workflow.id),
        data: json!({ "workflow": workflow }),
    })
}

fn edit(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    instruction: &str,
) -> Result<WorkflowCommandOutput> {
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    let staged = stage_existing_workflow(&ctx, &workflow)?;
    append_readme_note(&staged.path, "Edit request", instruction)?;
    let had_changes =
        finalize_staged_workflow_changes(&ctx, &staged, "Record workflow edit request")?;
    if had_changes && ctx.stage_session_id.is_none() {
        publish_staged_workflow(&staged.root.path, &staged.path, &staged.live_path)?;
    }
    let workflow = resolve_workflow_for_context(&ctx, id)?;
    Ok(WorkflowCommandOutput {
        message: format!("Recorded edit request for {}", workflow.id),
        data: json!({ "workflow": workflow }),
    })
}

fn fix(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    repair_workflow_command(ctx, id)
}

async fn run(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    input: Option<WorkflowInputSource>,
    input_fields: BTreeMap<String, String>,
) -> Result<WorkflowCommandOutput> {
    let workflows = discover_workflows_for_context(&ctx)?;
    let normalized_id = normalize_workflow_id(id)?;
    let workflow = resolve_workflow_for_context(&ctx, &normalized_id)?;
    let input = read_input(input, input_fields)?;
    let runtime_event_handler = standalone_cli_runtime_event_handler(ctx.progress);
    let output = workflow_runtime::run_workflow(
        ctx.codex_home,
        ctx.cwd,
        &workflow.path,
        &workflow.path.join("src/workflow.ts"),
        &input,
        workflow_runtime::WorkflowRuntimeRunOptions {
            workflows: &workflows,
            event_handler: runtime_event_handler.as_deref(),
        },
    )
    .await
    .with_context(|| format!("failed to run workflow {}", workflow.id))?;
    let stdout = output.stdout;
    let stderr = output.stderr;
    if !output.success {
        return Err(anyhow!(
            "workflow {} exited with {}\n{}",
            workflow.id,
            output.exit_status,
            stderr
        ));
    }
    Ok(WorkflowCommandOutput {
        message: stdout.clone(),
        data: json!({ "workflow": workflow, "stdout": stdout, "stderr": stderr }),
    })
}

fn config(
    ctx: WorkflowCommandContext<'_>,
    command: WorkflowConfigCommand,
) -> Result<WorkflowCommandOutput> {
    match command {
        WorkflowConfigCommand::Show => Ok(WorkflowCommandOutput {
            message: serde_json::to_string_pretty(&effective_config(ctx.config))?,
            data: json!({ "config": effective_config(ctx.config) }),
        }),
        WorkflowConfigCommand::Set { key, value } => {
            edit_workflows_config(ctx.codex_home, |table| {
                table[&key] = workflow_config_value(&key, &value)?;
                Ok(())
            })?;
            Ok(WorkflowCommandOutput {
                message: format!("Set workflows.{key}"),
                data: json!({ "key": key }),
            })
        }
        WorkflowConfigCommand::Clear { key } => {
            edit_workflows_config(ctx.codex_home, |table| {
                table.remove(&key);
                Ok(())
            })?;
            Ok(WorkflowCommandOutput {
                message: format!("Cleared workflows.{key}"),
                data: json!({ "key": key }),
            })
        }
    }
}

fn effective_config(config: &WorkflowsConfigToml) -> JsonValue {
    json!({
        "search_paths": config.search_paths.clone().unwrap_or_default(),
        "default_location": config.default_location.unwrap_or_default(),
        "repair_mode": config.repair_mode.clone().unwrap_or_else(|| "threshold:3".to_string()),
        "max_repair_cycles": config.max_repair_cycles.unwrap_or(DEFAULT_MAX_REPAIR_CYCLES),
        "dependency_update_policy": config.dependency_update_policy.clone().unwrap_or_else(|| "locked".to_string()),
        "commit_policy": config.commit_policy.clone().unwrap_or_else(|| "auto".to_string()),
        "validation_profile": config.validation_profile.clone().unwrap_or_else(|| "default".to_string()),
    })
}

pub fn discover_workflows_for_context(
    ctx: &WorkflowCommandContext<'_>,
) -> Result<Vec<WorkflowSummary>> {
    let mut workflows = discover_workflows(ctx.codex_home, ctx.cwd, ctx.config)?;
    if let Some(session_id) = ctx.stage_session_id.as_deref() {
        workflows.extend(discover_session_staged_workflows(ctx, session_id)?);
    }
    workflows.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.root_path.cmp(&right.root_path))
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(workflows)
}

pub fn resolve_workflow_for_context(
    ctx: &WorkflowCommandContext<'_>,
    id: &str,
) -> Result<WorkflowSummary> {
    let normalized_id = normalize_workflow_id(id)?;
    if let Some(workflow) = find_staged_workflow(ctx, &normalized_id)? {
        return Ok(workflow);
    }

    find_workflow(ctx.codex_home, ctx.cwd, ctx.config, &normalized_id).map_err(Into::into)
}

fn find_staged_workflow(
    ctx: &WorkflowCommandContext<'_>,
    normalized_id: &str,
) -> Result<Option<WorkflowSummary>> {
    let Some(session_id) = ctx.stage_session_id.as_deref() else {
        return Ok(None);
    };

    let workflows = discover_session_staged_workflows(ctx, session_id)?;
    let matches = workflows
        .into_iter()
        .filter(|workflow| workflow.id == normalized_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [workflow] => Ok(Some(workflow.clone())),
        workflows => Err(anyhow!(
            "workflow id '{}' exists in multiple staged roots: {:?}",
            normalized_id,
            workflows
                .iter()
                .map(|workflow| workflow.path.clone())
                .collect::<Vec<_>>()
        )),
    }
}

fn discover_session_staged_workflows(
    ctx: &WorkflowCommandContext<'_>,
    session_id: &str,
) -> Result<Vec<WorkflowSummary>> {
    let mut workflows = Vec::new();
    for root in workflow_roots(ctx.codex_home, ctx.cwd, ctx.config) {
        let session_root_path = session_stage_root_path(&root.path, session_id);
        if !session_root_path.is_dir() {
            continue;
        }

        let session_root = WorkflowRoot {
            kind: root.kind,
            label: root.label.clone(),
            path: session_root_path,
        };
        collect_workflows_recursive(
            ctx.codex_home,
            &session_root,
            &session_root.path,
            ctx.config,
            &mut workflows,
        )?;
    }
    Ok(workflows)
}

fn collect_workflows_recursive(
    codex_home: &Path,
    root: &WorkflowRoot,
    dir: &Path,
    config: &WorkflowsConfigToml,
    workflows: &mut Vec<WorkflowSummary>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    if dir.join(WORKFLOW_YAML).is_file() {
        if let Some(summary) = summarize_workflow(codex_home, root, dir, config) {
            workflows.push(summary);
        }
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || should_skip_stage_dir(&path) {
            continue;
        }
        collect_workflows_recursive(codex_home, root, &path, config, workflows)?;
    }
    Ok(())
}

fn should_skip_stage_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | ".workflow-staging" | "node_modules" | "target")
    )
}

fn publish_session_staged_workflows(
    ctx: &WorkflowCommandContext<'_>,
    session_id: &str,
) -> Result<()> {
    for root in workflow_roots(ctx.codex_home, ctx.cwd, ctx.config) {
        let session_root_path = session_stage_root_path(&root.path, session_id);
        if !session_root_path.is_dir() {
            continue;
        }

        let session_root = WorkflowRoot {
            kind: root.kind,
            label: root.label.clone(),
            path: session_root_path.clone(),
        };
        let staged_workflows = discover_session_staged_workflows(ctx, session_id)?;
        let staged_workflows = staged_workflows
            .into_iter()
            .filter(|workflow| workflow.root_path == session_root.path)
            .collect::<Vec<_>>();
        for staged_workflow in &staged_workflows {
            if let Some(reason) = workflow_quality_block_reason_for_workflow(staged_workflow)? {
                let live_path = root.path.join(
                    staged_workflow
                        .path
                        .strip_prefix(&session_root.path)
                        .with_context(|| {
                            format!(
                                "staged workflow {} is not under session root {}",
                                staged_workflow.path.display(),
                                session_root.path.display()
                            )
                        })?,
                );
                return Err(anyhow!(
                    "workflow changes failed validation and were not committed:\n{}",
                    remap_staged_workflow_reason(&reason, &staged_workflow.path, &live_path)
                ));
            }
        }

        for staged_workflow in staged_workflows {
            let live_path = root.path.join(
                staged_workflow
                    .path
                    .strip_prefix(&session_root.path)
                    .with_context(|| {
                        format!(
                            "staged workflow {} is not under session root {}",
                            staged_workflow.path.display(),
                            session_root.path.display()
                        )
                    })?,
            );
            publish_staged_workflow(&session_root.path, &staged_workflow.path, &live_path)?;
        }

        let _ = fs::remove_dir_all(&session_root.path);
    }

    Ok(())
}

fn discard_session_staged_workflows(
    ctx: &WorkflowCommandContext<'_>,
    session_id: &str,
) -> Result<()> {
    for root in workflow_roots(ctx.codex_home, ctx.cwd, ctx.config) {
        let session_root_path = session_stage_root_path(&root.path, session_id);
        if !session_root_path.is_dir() {
            continue;
        }

        fs::remove_dir_all(&session_root_path).with_context(|| {
            format!(
                "failed to remove workflow session staging root {}",
                session_root_path.display()
            )
        })?;
    }

    Ok(())
}

fn slugify(description: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in description.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
        if slug.len() >= 48 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "workflow".to_string()
    } else {
        slug.to_string()
    }
}

fn unique_slug_in_roots(roots: &[&Path], slug: &str) -> Result<String> {
    let mut candidate = slug.to_string();
    let mut suffix = 2;
    while roots.iter().any(|root| root.join(&candidate).exists()) {
        candidate = format!("{slug}-{suffix}");
        suffix += 1;
    }
    Ok(candidate)
}

fn title_from_description(description: &str) -> String {
    description
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or("Workflow")
        .to_string()
}

fn write_scaffold_files(path: &Path, id: &str, title: &str, description: &str) -> Result<()> {
    let command_label = id
        .split('/')
        .next_back()
        .filter(|command| !command.is_empty())
        .unwrap_or(id);
    let entrypoint_name = command_label.replace('-', "_");
    fs::write(
        path.join(".gitignore"),
        "node_modules/\ndist/\n.DS_Store\nartifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    fs::write(
        path.join("README.md"),
        format!(
            "# {title}\n\n{description}\n\n## Usage\n\n```sh\n/{command_label}\n# or\ncodex {command_label}\n```\n\n## Workflow Runtime\n\nPrefer `ctx.status({{ workflowName, workflowStatus, threads? }})` while the workflow is running so the TUI can render `Workflow <workflowName>: <workflowStatus>` with optional `-> <threadName>: <threadStatus>` rows when more than one thread is active. `ctx.progress(message, data?)` remains available as a legacy shorthand for single-string status updates. `ctx.runWorkflow(workflow, input?, {{ onStatusUpdate }})` can intercept child workflow status updates and either forward, transform, bundle, or suppress them. Export a named default async function for the execution entrypoint and keep the return value as the canonical JSON result. Use `WorkflowOutput.toTuiMarkdown(result)` for the markdown view when the workflow has a user-facing result.\n\n## Dependencies\n\nDo not rely on globally installed third-party packages. Built-in platform modules are fine, but every external package the workflow imports must be declared in this workflow's local `package.json` and resolved from this directory's `node_modules`.\n\n## Validation\n\nRun `codex workflow validate {id}` after changes and keep the validation commands, contract smoke output, docs, and coverage markers aligned with the workflow implementation.\n\n## Maintenance\n\nKeep `README.md`, `DESIGN.md`, `workflow.yaml`, and the test coverage markers in sync when workflow behavior changes. Update both docs together when the workflow contract changes. Keep generated or persistent runtime files under ignored `state/` or `artifacts/` paths.\n"
        ),
    )?;
    fs::write(
        path.join("DESIGN.md"),
        format!(
            "# {title} Design\n\n## Overview\n\nThis workflow is a local TypeScript package driven by Bun's TypeScript runtime and validated through `codex workflow validate {id}`.\n\n## Architecture\n\n- `src/workflow.ts` owns the named default async function, the typed workflow contract, autocomplete, and the optional markdown formatter.\n- `src/tests/` carries the coverage contract for positive, load, autocomplete, negative, and recovery paths.\n- `workflow.yaml` records validation commands, contract smoke input, and coverage expectations.\n- `state/` holds persistent runtime data; `artifacts/` holds generated run artifacts. Both are ignored except for `state/.gitkeep`.\n\n## Data Flow\n\n1. A registered workflow command loads the workflow from the local package through Bun.\n2. The workflow validates input, emits progress, and returns the canonical JSON result.\n3. `WorkflowOutput.toTuiMarkdown(result)` provides the markdown view for the TUI and workflow-to-workflow callers.\n4. `codex workflow validate {id}` runs the local validation commands, checks docs/layout/coverage markers, smoke-tests the output contract when configured, and publishes the contract only after validation passes.\n\n## Failure Handling\n\nValidate inputs early. Surface actionable failures instead of generic exit-only errors. When the workflow cannot satisfy its output contract, fail with a specific error before returning partial data.\n\n## Recovery Behavior\n\nPrefer recovery when correctness is preserved. Do not hide corruption or return misleading success. Set `validation.coverage.recovery` to `true` only when recovery exists and is tested.\n\n## Test Matrix\n\n- `src/tests/workflow.positive.test.ts`: positive path, progress, JSON result, and markdown companion coverage.\n- `src/tests/workflow.load.test.ts`: loadability smoke.\n- `src/tests/workflow.autocomplete.test.ts`: registry and command-completion readiness smoke.\n- `src/tests/workflow.negative.test.ts`: failure path and failure UX.\n- `src/tests/workflow.recovery.test.ts`: optional, only when recovery behavior exists.\n\n## Maintenance Notes\n\nKeep dependency usage local. Keep `// workflow-covers:` markers aligned with `validation.coverage`, including load and autocomplete. Update this file when the workflow behavior or review expectations change. Keep runtime state and generated artifacts out of git.\n"
        ),
    )?;
    fs::write(
        path.join("package.json"),
        format!(
            r#"{{
  "name": "{}",
  "private": true,
  "type": "module",
  "scripts": {{
    "build": "bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk",
    "test": "bun test src/tests",
    "run": "bun src/workflow.ts"
  }},
  "dependencies": {{
    "@openai/codex-sdk": "latest"
  }},
  "devDependencies": {{
    "@types/node": "latest",
    "typescript": "latest"
  }}
}}
"#,
            package_name(id)
        ),
    )?;
    fs::write(
        path.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noEmit": true
  },
  "include": ["src/**/*.ts"]
}
"#,
    )?;
    fs::write(
        path.join("src/workflow.ts"),
        format!(
            r#"import type {{ WorkflowContext }} from "@openai/codex-sdk/workflow";

export interface WorkflowInput {{
  input?: string;
}}

export interface WorkflowOutput {{
  ok: true;
  input: WorkflowInput;
}}

function validateInput(input: unknown): WorkflowInput {{
  if (!input || typeof input !== "object" || Array.isArray(input)) {{
    throw new Error("workflow input must be a JSON object");
  }}
  return input as WorkflowInput;
}}

export const WorkflowOutput = {{
  toTuiMarkdown(result: WorkflowOutput) {{
    return {{ markdown: "{markdown}" }};
  }},
}};

export default async function {entrypoint_name}(ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> {{
  const normalizedInput = validateInput(input);
  ctx.progress("Running workflow", {{ input: normalizedInput }});
  return {{ ok: true, input: normalizedInput }};
}}

export async function complete(_ctx: WorkflowContext) {{
  return [];
}}

if (import.meta.url === `file://${{process.argv[1]}}`) {{
  const inputIndex = process.argv.indexOf("--input");
  const rawInput = inputIndex >= 0 ? process.argv[inputIndex + 1] : "{{}}";
  const input = JSON.parse(rawInput ?? "{{}}");
  const output = await {entrypoint_name}({{
    progress() {{}},
    reportToUserMarkdown() {{}},
    status() {{}},
    runWorkflow() {{ throw new Error("runWorkflow() is unavailable in direct CLI smoke"); }},
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
  }} as never, input);
  console.log(JSON.stringify(output, null, 2));
}}
"#,
            markdown = escape_ts_string(&format!("# {title}\n\nWorkflow complete.")),
            entrypoint_name = entrypoint_name,
        ),
    )?;
    fs::write(
        path.join("src/tests/workflow.positive.test.ts"),
        format!(
            r#"// workflow-covers: positive progress finalResult
import assert from "node:assert/strict";
import test from "node:test";
import workflow, {{ WorkflowOutput }} from "../workflow.ts";

test("workflow reports progress and formats markdown", async () => {{
  const events: unknown[] = [];
  const output = await workflow({{
    progress(message: string, data: unknown) {{
      events.push(["progress", message, data]);
    }},
    reportToUserMarkdown(markdown: string) {{
      events.push(["report", markdown]);
    }},
    status() {{}},
    runWorkflow() {{ throw new Error("runWorkflow() is unavailable in unit tests"); }},
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
  }} as never, {{ input: "example" }});
  const formatted = WorkflowOutput.toTuiMarkdown(output);

  assert.deepEqual(output, {{ ok: true, input: {{ input: "example" }} }});
  assert.deepEqual(formatted, {{ markdown: "{markdown}" }});
  assert.deepEqual(events, [["progress", "Running workflow", {{ input: {{ input: "example" }} }}]]);
}});
"#,
            markdown = escape_ts_string(&format!("# {title}\n\nWorkflow complete.")),
        ),
    )?;
    fs::write(
        path.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )?;
    fs::write(
        path.join("src/tests/workflow.autocomplete.test.ts"),
        r#"// workflow-covers: autocomplete
import assert from "node:assert/strict";
import test from "node:test";
import { complete } from "../workflow.ts";

test("workflow exposes autocomplete", async () => {
  const suggestions = await complete({
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
    progress() {},
    status() {},
    reportToUserMarkdown() {},
    runWorkflow() { throw new Error("runWorkflow() is unavailable in unit tests"); },
  } as never);

  assert.deepEqual(suggestions, []);
});
"#,
    )?;
    fs::write(
        path.join("src/tests/workflow.negative.test.ts"),
        r#"// workflow-covers: negative failureUx
import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../workflow.ts";

test("workflow rejects invalid input", async () => {
  await assert.rejects(
    workflow({
      progress() {},
      reportToUserMarkdown() {},
      cwd: process.cwd(),
      currentWorkingDirectory: process.cwd(),
      repoRoot: process.cwd(),
      workingDirectory: process.cwd(),
      status() {},
      runWorkflow() { throw new Error("runWorkflow() is unavailable in unit tests"); },
    } as never, null),
    /workflow input must be a JSON object/
  );
});
"#,
    )?;
    fs::write(path.join("state/.gitkeep"), "")?;
    write_scaffold_runtime_stubs(path)?;
    Ok(())
}

fn write_scaffold_runtime_stubs(path: &Path) -> Result<()> {
    let node_modules = path.join("node_modules");
    let sdk_dir = node_modules.join("@openai/codex-sdk");
    let types_node_dir = node_modules.join("@types/node");
    let typescript_dir = node_modules.join("typescript");
    fs::create_dir_all(&sdk_dir)?;
    fs::create_dir_all(&types_node_dir)?;
    fs::create_dir_all(&typescript_dir)?;

    fs::write(
        sdk_dir.join("package.json"),
        r#"{
  "name": "@openai/codex-sdk",
  "private": true,
  "type": "module",
  "exports": {
    "./workflow": {
      "types": "./workflow.d.ts",
      "default": "./workflow.js"
    }
  }
}
"#,
    )?;
    fs::write(
        sdk_dir.join("workflow.d.ts"),
        r#"export interface WorkflowContext {
  cwd: string;
  currentWorkingDirectory: string;
  repoRoot: string;
  workingDirectory: string;
  progress(message: string, data?: unknown): void;
  reportToUserMarkdown(markdown: string): void;
  status(status: unknown): void;
  runWorkflow(workflow: string, input?: unknown, options?: unknown): Promise<unknown>;
}

export declare function defineWorkflow<T>(workflow: T): T;
export declare function runWorkflow(workflow: unknown, options?: unknown): Promise<unknown>;
"#,
    )?;
    fs::write(
        sdk_dir.join("workflow.js"),
        r#"export function defineWorkflow(workflow) {
  return workflow;
}

function defaultContext() {
  return {
    progress() {},
    reportToUserMarkdown() {},
    status() {},
  };
}

export async function runWorkflow(workflow, options = {}) {
  const input = typeof options === "object" && options !== null && "input" in options
    ? options.input
    : options;
  const ctx = typeof options === "object" && options !== null && "ctx" in options
    ? options.ctx
    : defaultContext();
  return workflow.run(ctx, input);
}
"#,
    )?;
    fs::write(
        types_node_dir.join("package.json"),
        "{\n  \"name\": \"@types/node\",\n  \"private\": true,\n  \"types\": \"./index.d.ts\"\n}\n",
    )?;
    fs::write(
        types_node_dir.join("index.d.ts"),
        r#"declare const process: {
  argv: string[];
  cwd(): string;
};
"#,
    )?;
    crate::api_contract::ensure_repo_typescript_shim(path)?;
    Ok(())
}

fn package_name(id: &str) -> String {
    format!("codex-workflow-{}", id.replace('/', "-"))
}

fn escape_ts_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('"', "\\\"")
}

fn append_readme_note(path: &Path, heading: &str, instruction: &str) -> Result<()> {
    let readme_path = path.join("README.md");
    let mut readme = fs::read_to_string(&readme_path).unwrap_or_default();
    if !readme.ends_with('\n') {
        readme.push('\n');
    }
    readme.push_str(&format!("\n## {heading}\n\n{instruction}\n"));
    fs::write(&readme_path, readme)
        .with_context(|| format!("failed to write {}", readme_path.display()))
}

fn read_input(
    input: Option<WorkflowInputSource>,
    input_fields: BTreeMap<String, String>,
) -> Result<String> {
    let input = match input {
        Some(WorkflowInputSource::Inline(input)) => input,
        Some(WorkflowInputSource::File(path)) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read workflow input {}", path.display()))?,
        None => "{}".to_string(),
    };
    if input_fields.is_empty() {
        return Ok(input);
    }

    let mut value: JsonValue = serde_json::from_str(&input)
        .with_context(|| "workflow input must be valid JSON when merging CLI input flags")?;
    let Some(object) = value.as_object_mut() else {
        return Err(anyhow!(
            "workflow input must be a JSON object when merging CLI input flags"
        ));
    };
    for (key, raw_value) in input_fields {
        object.insert(key, parse_input_field_value(&raw_value));
    }
    serde_json::to_string(&value).map_err(Into::into)
}

fn parse_input_field_value(raw_value: &str) -> JsonValue {
    serde_json::from_str(raw_value).unwrap_or_else(|_| JsonValue::String(raw_value.to_string()))
}

fn stage_existing_workflow(
    ctx: &WorkflowCommandContext<'_>,
    workflow: &WorkflowSummary,
) -> Result<StagedWorkflow> {
    let relative = workflow
        .path
        .strip_prefix(&workflow.root_path)
        .with_context(|| {
            format!(
                "workflow {} is not under root {}",
                workflow.path.display(),
                workflow.root_path.display()
            )
        })?;
    let live_root_path = live_root_path_for_workflow(ctx, workflow)?;
    let live_path = live_root_path.join(relative);
    let stage_root_path = match ctx.stage_session_id.as_deref() {
        Some(session_id) => create_session_stage_root(&live_root_path, session_id)?,
        None => create_stage_root(&live_root_path)?,
    };
    let staged_path = stage_root_path.join(relative);
    if !staged_path.exists() {
        copy_dir_recursive(&live_path, &staged_path)?;
    }

    Ok(StagedWorkflow {
        _guard: ctx
            .stage_session_id
            .is_none()
            .then(|| StageRootGuard::new(stage_root_path.clone())),
        root: WorkflowRoot {
            kind: workflow.root_kind,
            label: workflow.root_label.clone(),
            path: stage_root_path,
        },
        path: staged_path,
        live_path,
    })
}

fn finalize_staged_workflow_changes(
    ctx: &WorkflowCommandContext<'_>,
    staged: &StagedWorkflow,
    message: &str,
) -> Result<bool> {
    run_git(&staged.path, &["init"])?;

    let staged_workflow =
        summarize_workflow(ctx.codex_home, &staged.root, &staged.path, ctx.config).ok_or_else(
            || {
                anyhow!(
                    "failed to summarize staged workflow {}",
                    staged.path.display()
                )
            },
        )?;

    if let Some(reason) = workflow_quality_block_reason_for_workflow(&staged_workflow)? {
        return Err(anyhow!(
            "workflow changes failed validation and were not committed:\n{}",
            remap_staged_workflow_reason(&reason, &staged.path, &staged.live_path)
        ));
    }

    run_git(&staged.path, &["add", "."])?;
    let diff = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(&staged.path)
        .status()?;
    if diff.success() {
        return Ok(false);
    }

    let config = ctx.config;
    if matches!(
        config.commit_policy.as_deref(),
        Some("manual" | "none" | "disabled")
    ) {
        return Ok(true);
    }

    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(&staged.path)
        .env("GIT_AUTHOR_NAME", "Codex")
        .env("GIT_AUTHOR_EMAIL", "codex@openai.com")
        .env("GIT_COMMITTER_NAME", "Codex")
        .env("GIT_COMMITTER_EMAIL", "codex@openai.com")
        .output()?;
    if status.status.success() {
        Ok(true)
    } else {
        Err(anyhow!(
            "git commit failed with {}: {}{}",
            status.status,
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        ))
    }
}

fn remap_staged_workflow_reason(reason: &str, staged_path: &Path, live_path: &Path) -> String {
    reason.replace(
        &staged_path.display().to_string(),
        &live_path.display().to_string(),
    )
}

fn live_root_path_for_workflow(
    ctx: &WorkflowCommandContext<'_>,
    workflow: &WorkflowSummary,
) -> Result<PathBuf> {
    workflow_roots(ctx.codex_home, ctx.cwd, ctx.config)
        .into_iter()
        .find(|root| root.kind == workflow.root_kind && root.label == workflow.root_label)
        .map(|root| root.path)
        .ok_or_else(|| {
            anyhow!(
                "workflow root {} ({:?}) was not found",
                workflow.root_label,
                workflow.root_kind
            )
        })
}

#[cfg(test)]
fn commit_workflow_changes(
    ctx: &WorkflowCommandContext<'_>,
    path: &Path,
    message: &str,
) -> Result<()> {
    let config = ctx.config;
    if matches!(
        config.commit_policy.as_deref(),
        Some("manual" | "none" | "disabled")
    ) {
        return Ok(());
    }
    run_git(path, &["init"])?;
    if let Some(reason) =
        workflow_quality_block_reason_for_path(ctx.codex_home, ctx.cwd, ctx.config, path)?
    {
        return Err(anyhow!(
            "workflow changes failed validation and were not committed:\n{reason}"
        ));
    }
    run_git(path, &["add", "."])?;
    let diff = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(path)
        .status()?;
    if diff.success() {
        return Ok(());
    }
    let status = Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Codex")
        .env("GIT_AUTHOR_EMAIL", "codex@openai.com")
        .env("GIT_COMMITTER_NAME", "Codex")
        .env("GIT_COMMITTER_EMAIL", "codex@openai.com")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("git commit failed with {status}"))
    }
}

fn run_git(path: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git {} failed with {}: {}{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn edit_workflows_config<F>(codex_home: &Path, edit: F) -> Result<()>
where
    F: FnOnce(&mut Table) -> Result<()>,
{
    fs::create_dir_all(codex_home)?;
    let path = codex_home.join(CONFIG_TOML_FILE);
    let contents = fs::read_to_string(&path).unwrap_or_default();
    let mut document = contents.parse::<DocumentMut>().unwrap_or_default();
    if !document.as_table().contains_key("workflows") {
        document["workflows"] = Item::Table(Table::new());
    }
    let table = document["workflows"]
        .as_table_mut()
        .ok_or_else(|| anyhow!("[workflows] is not a table"))?;
    edit(table)?;
    fs::write(&path, document.to_string())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn workflow_config_value(key: &str, raw: &str) -> Result<Item> {
    match key {
        "max_repair_cycles" => Ok(value(i64::from(raw.parse::<u32>()?))),
        "search_paths" => {
            let mut array = Array::new();
            for path in raw
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                array.push(path);
            }
            Ok(Item::Value(array.into()))
        }
        "default_location"
        | "repair_mode"
        | "dependency_update_policy"
        | "commit_policy"
        | "validation_profile" => Ok(value(raw)),
        other => Err(anyhow!("unknown workflows config key '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_config::types::WorkflowDefaultLocation;
    #[cfg(unix)]
    use serial_test::serial;

    use pretty_assertions::assert_eq;

    use tempfile::TempDir;

    #[cfg(unix)]
    fn test_node_path() -> PathBuf {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path_env| std::env::split_paths(&path_env).collect::<Vec<_>>())
            .flat_map(|dir| [dir.join("node"), dir.join("nodejs")])
            .find(|candidate| candidate.is_file())
            .expect("node executable should be available for workflow tests")
    }

    fn write_validation_fixture(workflow_dir: &Path, validation_commands: JsonValue) {
        fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::write(
            workflow_dir.join(".gitignore"),
            "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("README.md"),
            "# Test\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("DESIGN.md"),
            "# Test Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "codex-workflow-review-fix",
  "private": true,
  "type": "module"
}
"#,
        )
        .unwrap();
        fs::write(workflow_dir.join("src/workflow.ts"), "export {};\n").unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
            "// workflow-covers: autocomplete\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        write_workflow_spec(
            &workflow_dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "review/fix".to_string(),
                api: json!({
                    "inputSchema": { "type": "object", "additionalProperties": true },
                    "outputSchema": {
                        "type": "object",
                        "properties": { "ok": { "type": "boolean" } },
                        "additionalProperties": true
                    }
                }),
                validation: json!({
                    "commands": validation_commands,
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let status = Command::new("git")
            .args(["init"])
            .current_dir(workflow_dir)
            .status()
            .unwrap();
        assert!(status.success(), "git init should succeed");
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@openai.com",
                "add",
                ".",
            ])
            .current_dir(workflow_dir)
            .status()
            .unwrap();
        assert!(status.success(), "git add should succeed");
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@openai.com",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(workflow_dir)
            .status()
            .unwrap();
        assert!(status.success(), "git commit should succeed");
    }

    #[test]
    fn develop_creates_git_backed_workflow() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            default_location: Some(WorkflowDefaultLocation::Project),
            commit_policy: Some("manual".to_string()),
            ..Default::default()
        };

        let output = execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &config,
                codex_self_exe: None,
                stage_session_id: None,
                progress: None,
            },
            WorkflowCommand::Develop {
                description: "Jira Summary".to_string(),
            },
        )
        .unwrap();

        assert_eq!(
            output.data["id"],
            JsonValue::String("jira-summary".to_string())
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/workflow.yaml")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/README.md")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/DESIGN.md")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/package.json")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/src/tests")
                .is_dir()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/src/tests/workflow.positive.test.ts")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/src/tests/workflow.load.test.ts")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/src/tests/workflow.autocomplete.test.ts")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/src/tests/workflow.negative.test.ts")
                .is_file()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/state")
                .is_dir()
        );
        assert!(
            cwd.path()
                .join(".codex/workflows/jira-summary/state/.gitkeep")
                .is_file()
        );
        let gitignore =
            fs::read_to_string(cwd.path().join(".codex/workflows/jira-summary/.gitignore"))
                .unwrap();
        assert!(gitignore.contains("artifacts/"));
        assert!(gitignore.contains("state/*"));
        assert!(gitignore.contains("!state/.gitkeep"));
        let spec = read_workflow_spec(
            &cwd.path()
                .join(".codex/workflows/jira-summary/workflow.yaml"),
        )
        .unwrap();
        assert_eq!(
            spec.validation["coverage"]["positive"],
            JsonValue::Bool(true)
        );
        assert_eq!(
            spec.validation["coverage"]["negative"],
            JsonValue::Bool(true)
        );
        assert_eq!(
            spec.validation["coverage"]["progress"],
            JsonValue::Bool(true)
        );
        assert_eq!(
            spec.validation["coverage"]["finalResult"],
            JsonValue::Bool(true)
        );
        assert_eq!(
            spec.validation["coverage"]["failureUx"],
            JsonValue::Bool(true)
        );
        assert_eq!(spec.validation["coverage"]["load"], JsonValue::Bool(true));
        assert_eq!(
            spec.validation["coverage"]["autocomplete"],
            JsonValue::Bool(true)
        );
        assert_eq!(
            spec.validation["coverage"]["recovery"],
            JsonValue::Bool(false)
        );
        assert_eq!(
            spec.validation["contractSmoke"]["input"]["input"],
            JsonValue::String("example".to_string())
        );
    }

    #[test]
    fn validate_workflow_runs_validation_commands() {
        let temp_dir = TempDir::new().unwrap();
        let workflow_dir = temp_dir.path().join("review/fix");
        write_validation_fixture(&workflow_dir, json!(["echo ok", "exit 0"]));
        let workflow = crate::registry::WorkflowSummary {
            id: "review/fix".to_string(),
            command: Some("fix".to_string()),
            title: Some("Fix".to_string()),
            user_description: Some("Fix workflow".to_string()),
            search_terms: Vec::new(),
            command_option_hints: Vec::new(),
            root_label: "global".to_string(),
            root_kind: crate::registry::WorkflowRootKind::Global,
            root_path: temp_dir.path().to_path_buf(),
            path: workflow_dir.clone(),
            workflow_yaml_path: workflow_dir.join(WORKFLOW_YAML),
            mention_target: "workflow:///tmp#review/fix".to_string(),
            validation: validate_workflow_dir(temp_dir.path(), &workflow_dir, "review/fix"),
            repair_mode: "threshold:3".to_string(),
        };

        let report = validate_workflow(&workflow, run_validation_command).unwrap();

        assert_eq!(
            report.status,
            crate::registry::WorkflowValidationStatus::Valid
        );
        assert_eq!(
            crate::validation_finding::finding_messages(&report.findings),
            Vec::<String>::new()
        );
        assert_eq!(report.command_results.len(), 2);
        assert_eq!(report.command_results[0].command, "echo ok");
        assert!(report.command_results[0].succeeded);
        assert_eq!(report.command_results[1].command, "exit 0");
        assert!(report.command_results[1].succeeded);
    }

    #[test]
    fn validate_workflow_reports_failing_validation_command() {
        let temp_dir = TempDir::new().unwrap();
        let workflow_dir = temp_dir.path().join("review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 1", "echo skipped"]));
        let workflow = crate::registry::WorkflowSummary {
            id: "review/fix".to_string(),
            command: Some("fix".to_string()),
            title: Some("Fix".to_string()),
            user_description: Some("Fix workflow".to_string()),
            search_terms: Vec::new(),
            command_option_hints: Vec::new(),
            root_label: "global".to_string(),
            root_kind: crate::registry::WorkflowRootKind::Global,
            root_path: temp_dir.path().to_path_buf(),
            path: workflow_dir.clone(),
            workflow_yaml_path: workflow_dir.join(WORKFLOW_YAML),
            mention_target: "workflow:///tmp#review/fix".to_string(),
            validation: validate_workflow_dir(temp_dir.path(), &workflow_dir, "review/fix"),
            repair_mode: "threshold:3".to_string(),
        };

        let report = validate_workflow(&workflow, run_validation_command).unwrap();

        assert_eq!(
            report.status,
            crate::registry::WorkflowValidationStatus::Invalid
        );
        assert_eq!(report.command_results.len(), 1);
        assert_eq!(
            crate::validation_finding::finding_messages(&report.findings),
            vec!["validation command `exit 1` failed with exit code 1".to_string()]
        );
    }

    #[test]
    fn commit_workflow_changes_refuses_invalid_workflow() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));
        fs::write(
            workflow_dir.join(WORKFLOW_YAML),
            "id: review/other\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
        )
        .unwrap();

        let before_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&workflow_dir)
            .output()
            .unwrap();
        assert!(before_head.status.success());
        let before_head = String::from_utf8(before_head.stdout).unwrap();

        let config = WorkflowsConfigToml::default();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: None,
            progress: None,
        };

        let err = commit_workflow_changes(&ctx, &workflow_dir, "Update workflow documentation")
            .expect_err("invalid workflow should not be committed");

        assert!(
            err.to_string()
                .contains("workflow changes failed validation and were not committed")
        );
        assert!(err.to_string().contains("[WF-007]"));

        let after_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&workflow_dir)
            .output()
            .unwrap();
        assert!(after_head.status.success());
        let after_head = String::from_utf8(after_head.stdout).unwrap();

        assert_eq!(after_head, before_head);
    }

    #[test]
    fn staged_workflow_changes_publish_after_validation() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));

        let config = WorkflowsConfigToml::default();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: None,
            progress: None,
        };
        let workflow = find_workflow(home.path(), cwd.path(), &config, "review/fix").unwrap();
        let staged = stage_existing_workflow(&ctx, &workflow).unwrap();

        let live_readme_before = fs::read_to_string(workflow.path.join("README.md")).unwrap();
        append_readme_note(&staged.path, "Documentation", "staged change").unwrap();

        let had_changes =
            finalize_staged_workflow_changes(&ctx, &staged, "Update workflow documentation")
                .unwrap();

        assert!(had_changes);
        assert_eq!(
            fs::read_to_string(workflow.path.join("README.md")).unwrap(),
            live_readme_before
        );

        publish_staged_workflow(&staged.root.path, &staged.path, &workflow.path).unwrap();

        let live_readme_after = fs::read_to_string(workflow.path.join("README.md")).unwrap();
        assert!(live_readme_after.contains("staged change"));
    }

    #[test]
    fn staged_workflow_changes_publish_only_on_done_for_session_staging() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));

        let config = WorkflowsConfigToml::default();
        let session_id = "019d0000-0000-0000-0000-000000000001".to_string();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: Some(session_id.clone()),
            progress: None,
        };

        let workflow = find_workflow(home.path(), cwd.path(), &config, "review/fix").unwrap();
        let live_root = default_workflow_root(home.path(), cwd.path(), &config);
        let session_stage_root = session_stage_root_path(&live_root.path, &session_id);
        let live_readme_before = fs::read_to_string(workflow.path.join("README.md")).unwrap();

        execute_workflow_command(
            ctx.clone(),
            WorkflowCommand::Docs {
                id: "review/fix".to_string(),
                instruction: "staged change".to_string(),
            },
        )
        .unwrap();

        assert_eq!(
            fs::read_to_string(workflow.path.join("README.md")).unwrap(),
            live_readme_before
        );
        assert!(session_stage_root.exists());

        execute_workflow_command(ctx, WorkflowCommand::Done).unwrap();

        assert!(!session_stage_root.exists());
        let live_readme_after = fs::read_to_string(workflow.path.join("README.md")).unwrap();
        assert!(live_readme_after.contains("staged change"));
    }

    #[test]
    fn staged_workflow_changes_publish_with_explicit_command() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));

        let config = WorkflowsConfigToml::default();
        let session_id = "019d0000-0000-0000-0000-000000000010".to_string();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: Some(session_id.clone()),
            progress: None,
        };

        let workflow = find_workflow(home.path(), cwd.path(), &config, "review/fix").unwrap();
        let live_root = default_workflow_root(home.path(), cwd.path(), &config);
        let session_stage_root = session_stage_root_path(&live_root.path, &session_id);

        execute_workflow_command(
            ctx.clone(),
            WorkflowCommand::Docs {
                id: "review/fix".to_string(),
                instruction: "published change".to_string(),
            },
        )
        .unwrap();
        assert!(session_stage_root.exists());

        execute_workflow_command(ctx, WorkflowCommand::Publish).unwrap();

        assert!(!session_stage_root.exists());
        let live_readme_after = fs::read_to_string(workflow.path.join("README.md")).unwrap();
        assert!(live_readme_after.contains("published change"));
    }

    #[test]
    fn staged_workflow_changes_discard_with_explicit_command() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));

        let config = WorkflowsConfigToml::default();
        let session_id = "019d0000-0000-0000-0000-000000000011".to_string();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: Some(session_id.clone()),
            progress: None,
        };

        let workflow = find_workflow(home.path(), cwd.path(), &config, "review/fix").unwrap();
        let live_root = default_workflow_root(home.path(), cwd.path(), &config);
        let session_stage_root = session_stage_root_path(&live_root.path, &session_id);
        let live_readme_before = fs::read_to_string(workflow.path.join("README.md")).unwrap();

        execute_workflow_command(
            ctx.clone(),
            WorkflowCommand::Docs {
                id: "review/fix".to_string(),
                instruction: "discarded change".to_string(),
            },
        )
        .unwrap();
        assert!(session_stage_root.exists());

        execute_workflow_command(ctx, WorkflowCommand::Discard).unwrap();

        assert!(!session_stage_root.exists());
        assert_eq!(
            fs::read_to_string(workflow.path.join("README.md")).unwrap(),
            live_readme_before
        );
    }

    #[test]
    fn staged_workflow_changes_reuse_the_same_session_stage_root() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/review/fix");
        write_validation_fixture(&workflow_dir, json!(["exit 0"]));

        let config = WorkflowsConfigToml::default();
        let session_id = "019d0000-0000-0000-0000-000000000002".to_string();
        let ctx = WorkflowCommandContext {
            codex_home: home.path(),
            cwd: cwd.path(),
            config: &config,
            codex_self_exe: None,
            stage_session_id: Some(session_id.clone()),
            progress: None,
        };

        let workflow = find_workflow(home.path(), cwd.path(), &config, "review/fix").unwrap();
        let live_root = default_workflow_root(home.path(), cwd.path(), &config);
        let session_stage_root = session_stage_root_path(&live_root.path, &session_id);

        execute_workflow_command(
            ctx.clone(),
            WorkflowCommand::Docs {
                id: "review/fix".to_string(),
                instruction: "first staged change".to_string(),
            },
        )
        .unwrap();
        execute_workflow_command(
            ctx.clone(),
            WorkflowCommand::Docs {
                id: "review/fix".to_string(),
                instruction: "second staged change".to_string(),
            },
        )
        .unwrap();

        assert!(session_stage_root.exists());
        assert!(!session_stage_root.join(".workflow-staging").exists());

        execute_workflow_command(ctx, WorkflowCommand::Done).unwrap();

        let live_readme_after = fs::read_to_string(workflow.path.join("README.md")).unwrap();
        assert!(live_readme_after.contains("first staged change"));
        assert!(live_readme_after.contains("second staged change"));
    }

    #[test]
    fn read_input_merges_cli_input_fields_into_empty_object() {
        let input = read_input(
            /*input*/ None,
            BTreeMap::from([
                ("reviewMode".to_string(), "initial".to_string()),
                ("scope".to_string(), "repo".to_string()),
                ("workingDirectory".to_string(), "/tmp/repo".to_string()),
            ]),
        )
        .unwrap();

        assert_eq!(
            serde_json::from_str::<JsonValue>(&input).unwrap(),
            json!({
                "reviewMode": "initial",
                "scope": "repo",
                "workingDirectory": "/tmp/repo",
            })
        );
    }

    #[test]
    fn read_input_cli_fields_override_existing_json_keys() {
        let input = read_input(
            Some(WorkflowInputSource::Inline(
                r#"{"scope":"pr","count":1}"#.to_string(),
            )),
            BTreeMap::from([
                ("count".to_string(), "2".to_string()),
                ("scope".to_string(), "review".to_string()),
            ]),
        )
        .unwrap();

        assert_eq!(
            serde_json::from_str::<JsonValue>(&input).unwrap(),
            json!({
                "count": 2,
                "scope": "review",
            })
        );
    }

    #[test]
    fn read_input_rejects_non_object_json_when_cli_fields_are_present() {
        let err = read_input(
            Some(WorkflowInputSource::Inline("[]".to_string())),
            BTreeMap::from([("scope".to_string(), "repo".to_string())]),
        )
        .expect_err("non-object workflow input should be rejected when merging flags");

        assert_eq!(
            err.to_string(),
            "workflow input must be a JSON object when merging CLI input flags"
        );
    }

    #[test]
    fn read_input_reads_file_before_merging_cli_fields() {
        let temp_dir = TempDir::new().unwrap();
        let input_path = temp_dir.path().join("input.json");
        fs::write(&input_path, r#"{"scope":"repo"}"#).unwrap();

        let input = read_input(
            Some(WorkflowInputSource::File(input_path)),
            BTreeMap::from([("reviewMode".to_string(), "initial".to_string())]),
        )
        .unwrap();

        assert_eq!(
            serde_json::from_str::<JsonValue>(&input).unwrap(),
            json!({
                "reviewMode": "initial",
                "scope": "repo",
            })
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn run_handles_workflow_runtime_markers_without_app_server_bridge() {
        use std::os::unix::fs::PermissionsExt;

        let env_key = "CODEX_WORKFLOW_RUNTIME_MODE";
        let previous = std::env::var_os(env_key);
        unsafe {
            std::env::set_var(env_key, "process");
        }

        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/reports/runtime-progress");
        fs::create_dir_all(workflow_dir.join("src")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::create_dir_all(workflow_dir.join("node_modules/.bin")).unwrap();
        fs::create_dir_all(workflow_dir.join(".git")).unwrap();
        fs::write(workflow_dir.join("README.md"), "# Runtime Progress\n").unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        fs::write(
            workflow_dir.join("src/helper.js"),
            r#"export function progressMessage() {
  return "Preparing review";
}
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            r#"import { progressMessage } from "./helper.js";

const workflow = {
  async run(ctx, input) {
    ctx.progress(progressMessage(), { prompt: input.prompt, stage: "testing" });
    ctx.reportToUserMarkdown(`# Workflow Result\n\n${input.prompt}`);
    return { workflowStatus: "done", prompt: input.prompt, nodePath: process.env.NODE_PATH ?? null };
  },
};

export default workflow;
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("node_modules/.bin/bun"),
            r#"#!/usr/bin/env node
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const [runner, ...args] = process.argv.slice(2);
if (args[0] === '--serve') {
  const result = spawnSync('node', [runner, ...args], { stdio: 'inherit' });
  process.exit(result.status ?? 1);
}
const workflowPathIndex = args.indexOf('--workflow-path');
if (workflowPathIndex === -1 || workflowPathIndex + 1 >= args.length) {
  console.error('missing --workflow-path');
  process.exit(1);
}
const workflowPath = args[workflowPathIndex + 1];
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'workflow-runtime-'));
const workflowDir = path.dirname(workflowPath);
const tmpWorkflowDir = path.join(tmpDir, path.basename(workflowDir));
fs.cpSync(workflowDir, tmpWorkflowDir, { recursive: true });
const tmpPath = path.join(tmpWorkflowDir, path.basename(workflowPath) + '.mjs');
fs.copyFileSync(workflowPath, tmpPath);
args[workflowPathIndex + 1] = tmpPath;
const result = spawnSync('node', [runner, ...args], { stdio: 'inherit' });
process.exit(result.status ?? 1);
"#,
        )
        .unwrap();
        fs::set_permissions(
            workflow_dir.join("node_modules/.bin/bun"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        write_workflow_spec(
            &workflow_dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "reports/runtime-progress".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        let output = execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &WorkflowsConfigToml::default(),
                codex_self_exe: None,
                stage_session_id: None,
                progress: None,
            },
            WorkflowCommand::Run {
                id: "reports/runtime-progress".to_string(),
                input: Some(WorkflowInputSource::Inline(
                    r#"{"prompt":"check status"}"#.to_string(),
                )),
                input_fields: BTreeMap::new(),
            },
        )
        .unwrap();

        match previous {
            Some(previous) => unsafe { std::env::set_var(env_key, previous) },
            None => unsafe { std::env::remove_var(env_key) },
        }

        assert!(output.message.contains("workflowStatus"));
        assert!(output.message.contains("check status"));
        assert!(output.message.contains("\"nodePath\": null"));
        assert_eq!(output.data["stderr"], json!(""));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn run_workflow_host_reuses_same_node_process_and_resets_module_state() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/reports/resident-host");
        fs::create_dir_all(workflow_dir.join("src")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::create_dir_all(workflow_dir.join("node_modules/.bin")).unwrap();
        fs::create_dir_all(workflow_dir.join(".git")).unwrap();
        fs::write(workflow_dir.join("README.md"), "# Resident Host\n").unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            r#"let runs = 0;

const workflow = {
  async run() {
    runs += 1;
    return { pid: process.pid, runs };
  },
};

export default workflow;
"#,
        )
        .unwrap();
        let host_log = workflow_dir.join("host-stderr.log");
        let node_path = test_node_path();
        write_workflow_spec(
            &workflow_dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "reports/resident-host".to_string(),
                ..Default::default()
            },
        )
        .unwrap();

        fs::write(
            workflow_dir.join("node_modules/.bin/bun"),
            format!(
                r#"#!{}
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {{ spawnSync }} = require('node:child_process');
const logPath = '{}';
const logFd = fs.openSync(logPath, 'a');
const [runner, ...args] = process.argv.slice(2);
if (args[0] === '--serve') {{
  const result = spawnSync(process.execPath, [runner, ...args], {{ stdio: ['ignore', logFd, logFd] }});
  process.exit(result.status ?? 1);
}}
const workflowPathIndex = args.indexOf('--workflow-path');
if (workflowPathIndex === -1 || workflowPathIndex + 1 >= args.length) {{
  fs.writeSync(logFd, 'missing --workflow-path\n');
  process.exit(1);
}}
const workflowPath = args[workflowPathIndex + 1];
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'workflow-runtime-'));
const workflowDir = path.dirname(workflowPath);
const tmpWorkflowDir = path.join(tmpDir, path.basename(workflowDir));
fs.cpSync(workflowDir, tmpWorkflowDir, {{ recursive: true }});
const tmpPath = path.join(tmpWorkflowDir, path.basename(workflowPath) + '.mjs');
fs.copyFileSync(workflowPath, tmpPath);
args[workflowPathIndex + 1] = tmpPath;
const result = spawnSync(process.execPath, [runner, ...args], {{ stdio: ['ignore', logFd, logFd] }});
process.exit(result.status ?? 1);
"#,
                node_path.display(),
                host_log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(
            workflow_dir.join("node_modules/.bin/bun"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let first = match execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &WorkflowsConfigToml::default(),
                codex_self_exe: None,
                stage_session_id: None,
                progress: None,
            },
            WorkflowCommand::Run {
                id: "reports/resident-host".to_string(),
                input: Some(WorkflowInputSource::Inline("{}".to_string())),
                input_fields: BTreeMap::new(),
            },
        ) {
            Ok(output) => output,
            Err(error) => panic!(
                "resident workflow host did not start: {error}\nhost stderr:\n{}",
                fs::read_to_string(&host_log).unwrap_or_default()
            ),
        };

        let second = match execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &WorkflowsConfigToml::default(),
                codex_self_exe: None,
                stage_session_id: None,
                progress: None,
            },
            WorkflowCommand::Run {
                id: "reports/resident-host".to_string(),
                input: Some(WorkflowInputSource::Inline("{}".to_string())),
                input_fields: BTreeMap::new(),
            },
        ) {
            Ok(output) => output,
            Err(error) => panic!(
                "resident workflow host did not restart cleanly: {error}\nhost stderr:\n{}",
                fs::read_to_string(&host_log).unwrap_or_default()
            ),
        };

        let first_result: JsonValue = serde_json::from_str(&first.message).unwrap();
        let second_result: JsonValue = serde_json::from_str(&second.message).unwrap();
        assert_eq!(first_result["pid"], second_result["pid"]);
        assert_eq!(first_result["runs"], json!(1));
        assert_eq!(second_result["runs"], json!(1));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn run_workflow_hook_can_transform_and_attach_child_status_updates() {
        use std::os::unix::fs::PermissionsExt;

        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/reports/parent-review");
        fs::create_dir_all(workflow_dir.join("src")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::create_dir_all(workflow_dir.join("node_modules/.bin")).unwrap();
        fs::create_dir_all(workflow_dir.join(".git")).unwrap();
        fs::write(workflow_dir.join("README.md"), "# Parent Review\n").unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            r##"const seen = [];

const workflow = {
  async run(ctx) {
    await ctx.runWorkflow("child-review", { prompt: "check child" }, {
      onStatusUpdate(update, helpers) {
        const combined = helpers.attachOriginalChildStatus({
          workflowName: "parent-review",
          workflowStatus: "coordinating",
          threads: [
            { name: "reviewer-a", status: update.workflowStatus },
            { name: "reviewer-b", status: "waiting" },
          ],
          childStatuses: [],
        });
        seen.push(combined);
        helpers.reportStatus(combined);
        return null;
      },
    });
    return { seen };
  },
};

export default workflow;
"##,
        )
        .unwrap();
        let node_path = test_node_path();
        fs::write(
            workflow_dir.join("node_modules/.bin/bun"),
            format!(
                r#"#!{}
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {{ spawnSync }} = require('node:child_process');
const [runner, ...args] = process.argv.slice(2);
const workflowPathIndex = args.indexOf('--workflow-path');
if (workflowPathIndex === -1 || workflowPathIndex + 1 >= args.length) {{
  console.error('missing --workflow-path');
  process.exit(1);
}}
const workflowPath = args[workflowPathIndex + 1];
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'workflow-runtime-'));
const workflowDir = path.dirname(workflowPath);
const tmpWorkflowDir = path.join(tmpDir, path.basename(workflowDir));
fs.cpSync(workflowDir, tmpWorkflowDir, {{ recursive: true }});
const tmpPath = path.join(tmpWorkflowDir, path.basename(workflowPath) + '.mjs');
fs.copyFileSync(workflowPath, tmpPath);
args[workflowPathIndex + 1] = tmpPath;
const result = spawnSync(process.execPath, [runner, ...args], {{ stdio: 'inherit' }});
process.exit(result.status ?? 1);
"#,
                node_path.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(
            workflow_dir.join("node_modules/.bin/bun"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        write_workflow_spec(
            &workflow_dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "reports/parent-review".to_string(),
                command: Some("parent-review".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let fake_codex = home.path().join("fake-codex.sh");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nprintf '%s\\n' '__CODEX_WORKFLOW_EVENT__{\"type\":\"status\",\"status\":{\"workflowName\":\"child-review\",\"workflowStatus\":\"scanning\",\"threads\":[],\"childStatuses\":[]}}' >&2\nprintf '%s\\n' '{\"ok\":true}'\n",
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();

        let env_key = "CODEX_WORKFLOW_SELF_EXE";
        let previous = std::env::var_os(env_key);
        unsafe {
            std::env::set_var(env_key, &fake_codex);
        }

        let runtime_mode_key = "CODEX_WORKFLOW_RUNTIME_MODE";
        let previous_runtime_mode = std::env::var_os(runtime_mode_key);
        unsafe {
            std::env::set_var(runtime_mode_key, "process");
        }

        let output = execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &WorkflowsConfigToml::default(),
                codex_self_exe: None,
                stage_session_id: None,
                progress: None,
            },
            WorkflowCommand::Run {
                id: "reports/parent-review".to_string(),
                input: Some(WorkflowInputSource::Inline("{}".to_string())),
                input_fields: BTreeMap::new(),
            },
        )
        .unwrap();

        match previous_runtime_mode {
            Some(previous_runtime_mode) => unsafe {
                std::env::set_var(runtime_mode_key, previous_runtime_mode)
            },
            None => unsafe { std::env::remove_var(runtime_mode_key) },
        }

        match previous {
            Some(previous) => unsafe { std::env::set_var(env_key, previous) },
            None => unsafe { std::env::remove_var(env_key) },
        }

        let result: JsonValue = serde_json::from_str(&output.message).unwrap();
        assert_eq!(result["seen"][0]["workflowName"], json!("parent-review"));
        assert_eq!(result["seen"][0]["workflowStatus"], json!("coordinating"));
        assert_eq!(result["seen"][0]["threads"][0]["name"], json!("reviewer-a"));
        assert_eq!(result["seen"][0]["threads"][0]["status"], json!("scanning"));
        assert_eq!(result["seen"][0]["threads"][1]["name"], json!("reviewer-b"));
        assert_eq!(result["seen"][0]["threads"][1]["status"], json!("waiting"));
        assert_eq!(
            result["seen"][0]["childStatuses"][0]["workflowName"],
            json!("child-review")
        );
        assert_eq!(
            result["seen"][0]["childStatuses"][0]["workflowStatus"],
            json!("scanning")
        );
        assert_eq!(output.data["stderr"], json!(""));
    }
}
