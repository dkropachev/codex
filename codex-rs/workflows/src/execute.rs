use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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
use crate::registry::DEFAULT_MAX_REPAIR_CYCLES;
use crate::registry::default_workflow_root;
use crate::registry::discover_workflows;
use crate::registry::find_workflow;
use crate::registry::validate_workflow_dir;
use crate::registry::workflow_impact;
use crate::spec::WORKFLOW_YAML;
use crate::spec::read_workflow_spec;
use crate::spec::scaffold_workflow_spec;
use crate::spec::write_workflow_spec;
use crate::workflow_runtime;

pub struct WorkflowCommandContext<'a> {
    pub codex_home: &'a Path,
    pub cwd: &'a Path,
    pub config: &'a WorkflowsConfigToml,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandOutput {
    pub message: String,
    pub data: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowValidationCommandResult {
    command: String,
    succeeded: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowValidationReport {
    status: crate::registry::WorkflowValidationStatus,
    messages: Vec<String>,
    command_results: Vec<WorkflowValidationCommandResult>,
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
        WorkflowCommand::Done => Ok(WorkflowCommandOutput {
            message: "Workflow Mode is done.".to_string(),
            data: json!({ "done": true }),
        }),
    }
}

fn show_mode(ctx: WorkflowCommandContext<'_>) -> Result<WorkflowCommandOutput> {
    let workflows = discover_workflows(ctx.codex_home, ctx.cwd, ctx.config)?;
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
    let workflows = discover_workflows(ctx.codex_home, ctx.cwd, ctx.config)?;
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
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let spec = read_workflow_spec(&workflow.workflow_yaml_path)?;
    Ok(WorkflowCommandOutput {
        message: serde_yaml::to_string(&spec)?,
        data: json!({ "workflow": workflow, "spec": spec }),
    })
}

fn where_workflow(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    Ok(WorkflowCommandOutput {
        message: workflow.path.display().to_string(),
        data: json!({ "workflow": workflow }),
    })
}

fn validate(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let report = validate_workflow(&workflow, run_validation_command)?;
    Ok(WorkflowCommandOutput {
        message: validation_report_message(&report),
        data: json!({ "workflow": workflow, "validation": report }),
    })
}

fn impact(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let impact = workflow_impact(&workflow)?;
    Ok(WorkflowCommandOutput {
        message: serde_json::to_string_pretty(&impact)?,
        data: json!({ "impact": impact }),
    })
}

fn status(ctx: WorkflowCommandContext<'_>, id: Option<&str>) -> Result<WorkflowCommandOutput> {
    if let Some(id) = id {
        let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
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

    let workflows = discover_workflows(ctx.codex_home, ctx.cwd, ctx.config)?;
    Ok(WorkflowCommandOutput {
        message: format!("{} workflow(s) discovered", workflows.len()),
        data: json!({ "workflows": workflows, "defaults": effective_config(ctx.config) }),
    })
}

fn develop(ctx: WorkflowCommandContext<'_>, description: &str) -> Result<WorkflowCommandOutput> {
    let root = default_workflow_root(ctx.codex_home, ctx.cwd, ctx.config);
    fs::create_dir_all(&root.path)
        .with_context(|| format!("failed to create workflow root {}", root.path.display()))?;
    let slug = unique_slug(&root.path, &slugify(description))?;
    let id = normalize_workflow_id(&slug)?;
    let path = root.path.join(&id);
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
    commit_workflow_changes(ctx.config, &path, "Create workflow scaffold")?;

    Ok(WorkflowCommandOutput {
        message: format!("Created workflow {id} at {}", path.display()),
        data: json!({ "id": id, "path": path }),
    })
}

fn describe(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    description: &str,
) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let mut spec = read_workflow_spec(&workflow.workflow_yaml_path)?;
    spec.user_description = Some(description.to_string());
    write_workflow_spec(&workflow.workflow_yaml_path, &spec)?;
    commit_workflow_changes(ctx.config, &workflow.path, "Update workflow description")?;
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
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    append_readme_note(&workflow.path, "Documentation", instruction)?;
    commit_workflow_changes(ctx.config, &workflow.path, "Update workflow documentation")?;
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
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    append_readme_note(&workflow.path, "Edit request", instruction)?;
    commit_workflow_changes(ctx.config, &workflow.path, "Record workflow edit request")?;
    Ok(WorkflowCommandOutput {
        message: format!("Recorded edit request for {}", workflow.id),
        data: json!({ "workflow": workflow }),
    })
}

fn fix(ctx: WorkflowCommandContext<'_>, id: &str) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let mut spec = read_workflow_spec(&workflow.workflow_yaml_path)?;
    let expected_id = workflow.id.clone();
    let mut changed = false;
    if spec.id != expected_id {
        spec.id = expected_id;
        changed = true;
    }
    if changed {
        write_workflow_spec(&workflow.workflow_yaml_path, &spec)?;
        commit_workflow_changes(ctx.config, &workflow.path, "Repair workflow metadata")?;
    }
    let validation = validate_workflow_dir(&workflow.root_path, &workflow.path, &workflow.id);
    Ok(WorkflowCommandOutput {
        message: validation_message(&validation),
        data: json!({ "workflow": workflow, "validation": validation, "changed": changed }),
    })
}

async fn run(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    input: Option<WorkflowInputSource>,
    input_fields: BTreeMap<String, String>,
) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let input = read_input(input, input_fields)?;
    let output = workflow_runtime::run_workflow(
        &workflow.path,
        &workflow.path.join("src/workflow.ts"),
        &input,
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

fn validation_message(validation: &crate::registry::WorkflowValidation) -> String {
    if validation.messages.is_empty() {
        "valid".to_string()
    } else {
        validation.messages.join("\n")
    }
}

fn validation_report_message(report: &WorkflowValidationReport) -> String {
    if report.messages.is_empty() {
        "valid".to_string()
    } else {
        report.messages.join("\n")
    }
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

fn unique_slug(root: &Path, slug: &str) -> Result<String> {
    let mut candidate = slug.to_string();
    let mut suffix = 2;
    while root.join(&candidate).exists() {
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
    fs::write(
        path.join("README.md"),
        format!(
            "# {title}\n\n{description}\n\n## Usage\n\n```sh\ncodex workflow run {id} --key value\n# or\ncodex workflow run {id} --input '{{}}'\n```\n\n## Workflow Runtime\n\nUse `ctx.progress(message, data?)` while the workflow is running so the TUI can keep the live workflow status row up to date. Use `ctx.reportToUserMarkdown(markdown)` only when the workflow should hand markdown back to the next plain user turn in the TUI.\n\n## Dependencies\n\nDo not rely on globally installed third-party packages. Built-in platform modules are fine, but every external package the workflow imports must be declared in this workflow's local `package.json` and resolved from this directory's `node_modules`.\n"
        ),
    )?;
    fs::write(
        path.join("DESIGN.md"),
        format!(
            "# {title} Design\n\n## Overview\n\nThis workflow is a local TypeScript package driven by `tsx` and validated through `codex workflow validate {id}`.\n\n## Architecture\n\n- `src/workflow.ts` owns the runtime behavior.\n- `src/tests/` carries the coverage contract for positive, negative, and recovery paths.\n- `workflow.yaml` records validation commands and coverage expectations.\n- `state/` holds any persistent data.\n\n## Data Flow\n\n1. `codex workflow run {id}` loads the workflow from the local package.\n2. The workflow validates input, emits progress, and reports markdown when it has a user-facing result.\n3. `codex workflow validate {id}` runs the local validation commands and checks the required docs, layout, and coverage markers.\n\n## Failure Handling\n\nValidate inputs early. Surface actionable failures instead of generic exit-only errors.\n\n## Recovery Behavior\n\nPrefer recovery when correctness is preserved. Do not hide corruption or return misleading success. Set `validation.coverage.recovery` to `true` only when recovery exists and is tested.\n\n## Test Matrix\n\n- `src/tests/workflow.positive.test.ts`: positive path, progress, and final markdown handoff.\n- `src/tests/workflow.negative.test.ts`: failure path and failure UX.\n- `src/tests/workflow.recovery.test.ts`: optional, only when recovery behavior exists.\n\n## Maintenance Notes\n\nKeep dependency usage local. Keep `// workflow-covers:` markers aligned with `validation.coverage`. Update this file when the workflow behavior or review expectations change.\n"
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
    "build": "tsc --noEmit",
    "test": "node --import tsx --test src/tests/**/*.test.ts",
    "run": "tsx src/workflow.ts"
  }},
  "dependencies": {{
    "@openai/codex-sdk": "latest"
  }},
  "devDependencies": {{
    "@types/node": "latest",
    "tsx": "latest",
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
            r#"import {{ defineWorkflow, runWorkflow }} from "@openai/codex-sdk/workflow";

function validateInput(input: unknown) {{
  if (!input || typeof input !== "object" || Array.isArray(input)) {{
    throw new Error("workflow input must be a JSON object");
  }}
  return input;
}}

const workflow = defineWorkflow({{
  id: "{id}",
  title: "{title}",
  description: "{description}",
  async run(ctx, input) {{
    const normalizedInput = validateInput(input);
    ctx.progress("Running workflow", {{ input: normalizedInput }});
    ctx.reportToUserMarkdown("{markdown}");
    return {{ ok: true, input: normalizedInput }};
  }},
}});

export default workflow;

if (import.meta.url === `file://${{process.argv[1]}}`) {{
  const inputIndex = process.argv.indexOf("--input");
  const rawInput = inputIndex >= 0 ? process.argv[inputIndex + 1] : "{{}}";
  const input = JSON.parse(rawInput ?? "{{}}");
  const output = await runWorkflow(workflow, {{ input }});
  console.log(JSON.stringify(output, null, 2));
}}
"#,
            id = escape_ts_string(id),
            title = escape_ts_string(title),
            description = escape_ts_string(description),
            markdown = escape_ts_string(&format!("# {title}\n\nWorkflow complete.")),
        ),
    )?;
    fs::write(
        path.join("src/tests/workflow.positive.test.ts"),
        format!(
            r#"// workflow-covers: positive progress finalResult
import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../workflow.js";

test("workflow reports progress and markdown", async () => {{
  const events: unknown[] = [];
  const output = await workflow.run({{
    progress(message: string, data: unknown) {{
      events.push(["progress", message, data]);
    }},
    reportToUserMarkdown(markdown: string) {{
      events.push(["report", markdown]);
    }},
  }}, {{ input: "example" }});

  assert.deepEqual(output, {{ ok: true, input: {{ input: "example" }} }});
  assert.deepEqual(events, [
    ["progress", "Running workflow", {{ input: {{ input: "example" }} }}],
    ["report", "{markdown}"],
  ]);
}});
"#,
            markdown = escape_ts_string(&format!("# {title}\n\nWorkflow complete.")),
        ),
    )?;
    fs::write(
        path.join("src/tests/workflow.negative.test.ts"),
        r#"// workflow-covers: negative failureUx
import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../workflow.js";

test("workflow rejects invalid input", async () => {
  await assert.rejects(
    workflow.run({
      progress() {},
      reportToUserMarkdown() {},
    }, null),
    /workflow input must be a JSON object/
  );
});
"#,
    )?;
    fs::write(path.join("state/.gitkeep"), "")?;
    Ok(())
}

fn package_name(id: &str) -> String {
    format!("codex-workflow-{}", id.replace('/', "-"))
}

fn escape_ts_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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

fn validate_workflow<F>(
    workflow: &crate::registry::WorkflowSummary,
    mut command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let mut messages = workflow.validation.messages.clone();
    let mut command_results = Vec::new();

    if let Ok(spec) = read_workflow_spec(&workflow.workflow_yaml_path) {
        for command in validation_commands(&spec) {
            let result = command_runner(&command, &workflow.path)?;
            let command_failed = !result.succeeded;
            if command_failed {
                messages.push(format!(
                    "validation command `{command}` failed with {}",
                    exit_status_label(result.exit_code)
                ));
            }
            command_results.push(result);
            if command_failed {
                break;
            }
        }
    }

    let status = if messages.is_empty() {
        crate::registry::WorkflowValidationStatus::Valid
    } else {
        crate::registry::WorkflowValidationStatus::Invalid
    };
    Ok(WorkflowValidationReport {
        status,
        messages,
        command_results,
    })
}

fn validation_commands(spec: &crate::spec::WorkflowSpec) -> Vec<String> {
    let commands = spec
        .validation
        .get("commands")
        .and_then(JsonValue::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if commands.is_empty() {
        vec!["npm test".to_string()]
    } else {
        commands
    }
}

fn run_validation_command(command: &str, cwd: &Path) -> Result<WorkflowValidationCommandResult> {
    let output = validation_shell_command(command)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run validation command `{command}`"))?;
    Ok(WorkflowValidationCommandResult {
        command: command.to_string(),
        succeeded: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn validation_shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut process = Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = Command::new("sh");
        process.args(["-lc", command]);
        process
    }
}

fn exit_status_label(exit_code: Option<i32>) -> String {
    exit_code
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "a non-zero status".to_string())
}

fn commit_workflow_changes(config: &WorkflowsConfigToml, path: &Path, message: &str) -> Result<()> {
    if matches!(
        config.commit_policy.as_deref(),
        Some("manual" | "none" | "disabled")
    ) {
        return Ok(());
    }
    run_git(path, &["init"])?;
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
    let status = Command::new("git").args(args).current_dir(path).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("git {} failed with {status}", args.join(" ")))
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
    use futures::SinkExt;
    use futures::StreamExt;
    use pretty_assertions::assert_eq;
    use serial_test::serial;
    use std::fs::Permissions;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    fn write_validation_fixture(workflow_dir: &Path, validation_commands: JsonValue) {
        fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::create_dir_all(workflow_dir.join(".git")).unwrap();
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
            workflow_dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        write_workflow_spec(
            &workflow_dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "review/fix".to_string(),
                validation: json!({
                    "commands": validation_commands,
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();
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
        assert_eq!(
            spec.validation["coverage"]["recovery"],
            JsonValue::Bool(false)
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
        assert_eq!(report.messages, Vec::<String>::new());
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
            report.messages,
            vec!["validation command `exit 1` failed with exit code 1".to_string()]
        );
    }

    #[test]
    fn read_input_merges_cli_input_fields_into_empty_object() {
        let input = read_input(
            None,
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
    #[tokio::test(flavor = "multi_thread")]
    #[serial(workflow_runtime_notifications)]
    async fn run_forwards_workflow_runtime_notifications_to_app_server() {
        use std::os::unix::fs::PermissionsExt;

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
            workflow_dir.join("src/workflow.ts"),
            r#"const workflow = {
  async run(ctx, input) {
    ctx.progress("Preparing review", { prompt: input.prompt, stage: "testing" });
    ctx.reportToUserMarkdown(`# Workflow Result\n\n${input.prompt}`);
    return { workflowStatus: "done", prompt: input.prompt, nodePath: process.env.NODE_PATH ?? null };
  },
};

export default workflow;
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("node_modules/.bin/tsx"),
            "#!/bin/sh\nrunner=\"$1\"\nworkflow_flag=\"$2\"\nworkflow_path=\"$3\"\ninput_flag=\"$4\"\ninput_value=\"$5\"\ntmp=$(mktemp \"${TMPDIR:-/tmp}/workflow-runtime-XXXXXX.mjs\")\ncp \"$workflow_path\" \"$tmp\"\nexec node \"$runner\" \"$workflow_flag\" \"$tmp\" \"$input_flag\" \"$input_value\"\n",
        )
        .unwrap();
        fs::set_permissions(
            workflow_dir.join("node_modules/.bin/tsx"),
            Permissions::from_mode(0o755),
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

        let (websocket_url, server_task) = start_workflow_notification_server().await;
        let _app_server_url = ScopedEnvVar::set("CODEX_WORKFLOW_APP_SERVER_URL", &websocket_url);
        let _run_id = ScopedEnvVar::set("CODEX_WORKFLOW_RUN_ID", "run-123");
        let _thread_id = ScopedEnvVar::set("CODEX_WORKFLOW_ORIGIN_THREAD_ID", "thread-456");
        let _node_path = ScopedEnvVar::set("NODE_PATH", "/tmp/global-modules");

        let output = execute_workflow_command(
            WorkflowCommandContext {
                codex_home: home.path(),
                cwd: cwd.path(),
                config: &WorkflowsConfigToml::default(),
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

        assert!(output.message.contains("workflowStatus"));
        assert!(output.message.contains("check status"));
        assert!(output.message.contains("\"nodePath\": null"));

        let notifications = server_task.await.unwrap();
        assert_eq!(notifications.len(), 2);
        assert_eq!(notifications[0]["method"], "workflow/progress");
        assert_eq!(notifications[0]["params"]["runId"], "run-123");
        assert_eq!(notifications[0]["params"]["threadId"], "thread-456");
        assert_eq!(notifications[0]["params"]["message"], "Preparing review");
        assert_eq!(notifications[0]["params"]["data"]["stage"], "testing");
        assert_eq!(notifications[1]["method"], "workflow/reportToUserMarkdown");
        assert_eq!(notifications[1]["params"]["runId"], "run-123");
        assert_eq!(notifications[1]["params"]["threadId"], "thread-456");
        assert!(
            notifications[1]["params"]["markdown"]
                .as_str()
                .unwrap()
                .contains("Workflow Result")
        );
    }

    #[cfg(unix)]
    async fn start_workflow_notification_server()
    -> (String, tokio::task::JoinHandle<Vec<JsonValue>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_async(stream).await.unwrap();
            let initialize = read_text_message(&mut websocket).await;
            assert_eq!(initialize["method"], "initialize");
            websocket
                .send(Message::Text(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": initialize["id"].clone(),
                        "result": {},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let initialized = read_text_message(&mut websocket).await;
            assert_eq!(initialized["method"], "initialized");

            let mut notifications = Vec::new();
            while notifications.len() < 2 {
                let message = read_text_message(&mut websocket).await;
                let method = message["method"].as_str().unwrap_or_default();
                if method == "workflow/progress" || method == "workflow/reportToUserMarkdown" {
                    notifications.push(message);
                }
            }
            notifications
        });
        (format!("ws://{address}"), task)
    }

    #[cfg(unix)]
    async fn read_text_message(
        websocket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> JsonValue {
        loop {
            let frame = websocket.next().await.unwrap().unwrap();
            match frame {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
                    continue;
                }
                Message::Close(_) => panic!("unexpected close frame"),
            }
        }
    }

    #[cfg(unix)]
    struct ScopedEnvVar {
        key: &'static str,
        original: Option<String>,
    }

    #[cfg(unix)]
    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: this test is serialized because environment mutation is process-global.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    #[cfg(unix)]
    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match self.original.as_deref() {
                Some(value) => {
                    // SAFETY: this test is serialized because environment mutation is process-global.
                    unsafe {
                        std::env::set_var(self.key, value);
                    }
                }
                None => {
                    // SAFETY: this test is serialized because environment mutation is process-global.
                    unsafe {
                        std::env::remove_var(self.key);
                    }
                }
            }
        }
    }
}
