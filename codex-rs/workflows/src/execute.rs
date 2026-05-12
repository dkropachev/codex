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

pub fn execute_workflow_command(
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
        WorkflowCommand::Run { id, input } => run(ctx, &id, input),
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
    Ok(WorkflowCommandOutput {
        message: validation_message(&workflow.validation),
        data: json!({ "workflow": workflow }),
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
    fs::create_dir_all(path.join("tests"))?;

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

fn run(
    ctx: WorkflowCommandContext<'_>,
    id: &str,
    input: Option<WorkflowInputSource>,
) -> Result<WorkflowCommandOutput> {
    let workflow = find_workflow(ctx.codex_home, ctx.cwd, ctx.config, id)?;
    let input = read_input(input)?;
    let output = Command::new("npm")
        .args(["run", "run", "--", "--input", &input])
        .current_dir(&workflow.path)
        .output()
        .with_context(|| format!("failed to run workflow {}", workflow.id))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        return Err(anyhow!(
            "workflow {} exited with {}\n{}",
            workflow.id,
            output.status,
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
            "# {title}\n\n{description}\n\n## Usage\n\n```sh\ncodex workflow run {id} --input '{{}}'\n```\n"
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
    "test": "node --test",
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
  "include": ["src/**/*.ts", "tests/**/*.ts"]
}
"#,
    )?;
    fs::write(
        path.join("src/workflow.ts"),
        format!(
            r#"import {{ defineWorkflow, runWorkflow }} from "@openai/codex-sdk/workflow";

const workflow = defineWorkflow({{
  id: "{id}",
  title: "{title}",
  description: "{description}",
  async run(_ctx, input) {{
    return {{ ok: true, input }};
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
            description = escape_ts_string(description)
        ),
    )?;
    fs::write(
        path.join("tests/workflow.test.ts"),
        r#"import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../src/workflow.js";

test("workflow is defined", () => {
  assert.equal(typeof workflow, "object");
});
"#,
    )?;
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

fn read_input(input: Option<WorkflowInputSource>) -> Result<String> {
    match input {
        Some(WorkflowInputSource::Inline(input)) => Ok(input),
        Some(WorkflowInputSource::File(path)) => fs::read_to_string(&path)
            .with_context(|| format!("failed to read workflow input {}", path.display())),
        None => Ok("{}".to_string()),
    }
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
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

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
    }
}
