use std::collections::BTreeSet;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use codex_config::types::WorkflowDefaultLocation;
use codex_config::types::WorkflowsConfigToml;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use thiserror::Error;

use crate::command_completion::WorkflowCommandOptionHint;
use crate::command_completion::command_option_hints_from_spec;
use crate::id::mention_target;
use crate::id::normalize_workflow_id;
use crate::spec::WORKFLOW_YAML;
use crate::spec::WorkflowHookKind;
use crate::spec::WorkflowToolSpec;
use crate::spec::read_workflow_spec;
use crate::spec::workflow_tool_name;

pub const DEFAULT_REPAIR_MODE: &str = "threshold:3";
pub const DEFAULT_MAX_REPAIR_CYCLES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRootKind {
    Global,
    Project,
    SearchPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRoot {
    pub kind: WorkflowRootKind,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowValidationStatus {
    Valid,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowValidation {
    pub status: WorkflowValidationStatus,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSummary {
    pub id: String,
    pub command: Option<String>,
    pub title: Option<String>,
    pub user_description: Option<String>,
    pub search_terms: Vec<String>,
    #[serde(default)]
    pub command_option_hints: Vec<WorkflowCommandOptionHint>,
    pub root_label: String,
    pub root_kind: WorkflowRootKind,
    pub root_path: PathBuf,
    pub path: PathBuf,
    pub workflow_yaml_path: PathBuf,
    pub mention_target: String,
    pub validation: WorkflowValidation,
    pub repair_mode: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPublishedTool {
    pub workflow: WorkflowSummary,
    pub tool: WorkflowToolSpec,
}

impl WorkflowPublishedTool {
    pub fn tool_name(&self) -> String {
        workflow_tool_name(&self.workflow.id)
    }

    pub fn to_dynamic_tool_spec(&self) -> DynamicToolSpec {
        DynamicToolSpec {
            namespace: None,
            name: self.tool_name(),
            description: self.tool.description.clone(),
            input_schema: self.tool.input_schema.clone(),
            defer_loading: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowImpact {
    pub id: String,
    pub path: PathBuf,
    pub dependencies: Vec<String>,
    pub dev_dependencies: Vec<String>,
    pub git_status: Vec<String>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("workflow id '{id}' exists in multiple roots: {paths:?}")]
pub struct DuplicateWorkflowId {
    pub id: String,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Error)]
pub enum WorkflowRegistryError {
    #[error(transparent)]
    Duplicate(#[from] DuplicateWorkflowId),
    #[error("workflow id '{0}' was not found")]
    NotFound(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub fn workflow_roots(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Vec<WorkflowRoot> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    push_root(
        &mut roots,
        &mut seen,
        WorkflowRootKind::Global,
        "global".to_string(),
        AbsolutePathBuf::resolve_path_against_base("workflows", codex_home).to_path_buf(),
    );
    push_root(
        &mut roots,
        &mut seen,
        WorkflowRootKind::Project,
        "project".to_string(),
        AbsolutePathBuf::resolve_path_against_base(".codex/workflows", cwd).to_path_buf(),
    );

    for (index, search_path) in config
        .search_paths
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        push_root(
            &mut roots,
            &mut seen,
            WorkflowRootKind::SearchPath,
            format!("search:{}", index + 1),
            AbsolutePathBuf::resolve_path_against_base(search_path, cwd).to_path_buf(),
        );
    }

    roots
}

pub fn discover_workflows(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Vec<WorkflowSummary>> {
    let mut workflows = Vec::new();
    for root in workflow_roots(codex_home, cwd, config) {
        collect_workflows(&root, &root.path, config, &mut workflows)?;
    }
    workflows.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.root_path.cmp(&right.root_path))
    });
    Ok(workflows)
}

pub fn find_workflow(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    id: &str,
) -> Result<WorkflowSummary, WorkflowRegistryError> {
    let id = normalize_workflow_id(id).map_err(anyhow::Error::from)?;
    let matches = discover_workflows(codex_home, cwd, config)
        .map_err(WorkflowRegistryError::Other)?
        .into_iter()
        .filter(|workflow| workflow.id == id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(WorkflowRegistryError::NotFound(id)),
        [workflow] => Ok(workflow.clone()),
        workflows => Err(DuplicateWorkflowId {
            id,
            paths: workflows
                .iter()
                .map(|workflow| workflow.path.clone())
                .collect(),
        }
        .into()),
    }
}

pub fn validate_workflow_dir(
    root: &Path,
    workflow_dir: &Path,
    expected_id: &str,
) -> WorkflowValidation {
    crate::validation::validate_workflow_dir(root, workflow_dir, expected_id)
}

pub fn workflow_impact(summary: &WorkflowSummary) -> Result<WorkflowImpact> {
    let package_json = summary.path.join("package.json");
    let package: JsonValue = if package_json.is_file() {
        serde_json::from_str(&fs::read_to_string(&package_json).with_context(|| {
            format!("failed to read package manifest {}", package_json.display())
        })?)
        .with_context(|| {
            format!(
                "failed to parse package manifest {}",
                package_json.display()
            )
        })?
    } else {
        json!({})
    };

    Ok(WorkflowImpact {
        id: summary.id.clone(),
        path: summary.path.clone(),
        dependencies: package_dependency_names(&package, "dependencies"),
        dev_dependencies: package_dependency_names(&package, "devDependencies"),
        git_status: git_status_lines(&summary.path),
    })
}

pub fn discover_workflow_tools(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Vec<WorkflowPublishedTool>> {
    crate::publication::discover_workflow_tools(codex_home, cwd, config)
}

pub fn discover_workflow_tools_for_hook(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowPublishedTool>> {
    crate::publication::discover_workflow_tools_for_hook(codex_home, cwd, config, hook)
}

pub(crate) fn discover_workflow_tools_from_filesystem_for_hook(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    hook: WorkflowHookKind,
) -> Result<Vec<WorkflowPublishedTool>> {
    let mut tools = Vec::new();
    for workflow in discover_workflows(codex_home, cwd, config)? {
        let Ok(spec) = read_workflow_spec(&workflow.workflow_yaml_path) else {
            continue;
        };
        let Some(tool) = spec.tool else {
            continue;
        };
        if !tool.register_on.contains(&hook) {
            continue;
        }
        tools.push(WorkflowPublishedTool { workflow, tool });
    }
    tools.sort_by(|left, right| {
        left.workflow
            .id
            .cmp(&right.workflow.id)
            .then_with(|| left.workflow.root_path.cmp(&right.workflow.root_path))
    });
    Ok(tools)
}

fn push_root(
    roots: &mut Vec<WorkflowRoot>,
    seen: &mut BTreeSet<PathBuf>,
    kind: WorkflowRootKind,
    label: String,
    path: PathBuf,
) {
    if seen.insert(path.clone()) {
        roots.push(WorkflowRoot { kind, label, path });
    }
}

fn collect_workflows(
    root: &WorkflowRoot,
    dir: &Path,
    config: &WorkflowsConfigToml,
    workflows: &mut Vec<WorkflowSummary>,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    if dir.join(WORKFLOW_YAML).is_file() {
        if let Some(summary) = summarize_workflow(root, dir, config) {
            workflows.push(summary);
        }
        return Ok(());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || should_skip_dir(&path) {
            continue;
        }
        collect_workflows(root, &path, config, workflows)?;
    }
    Ok(())
}

pub(crate) fn summarize_workflow(
    root: &WorkflowRoot,
    workflow_dir: &Path,
    config: &WorkflowsConfigToml,
) -> Option<WorkflowSummary> {
    let relative = workflow_dir.strip_prefix(&root.path).ok()?;
    let id = normalize_workflow_id(&relative_workflow_id(relative)?).ok()?;
    let workflow_yaml_path = workflow_dir.join(WORKFLOW_YAML);
    let spec = read_workflow_spec(&workflow_yaml_path).unwrap_or_default();
    let repair_mode = spec
        .repair
        .as_ref()
        .and_then(|repair| repair.mode.clone())
        .or_else(|| config.repair_mode.clone())
        .unwrap_or_else(|| DEFAULT_REPAIR_MODE.to_string());
    let command_option_hints = command_option_hints_from_spec(&spec);
    let command =
        normalize_workflow_command(spec.command).or_else(|| default_workflow_command(&id));
    let validation = validate_workflow_dir(&root.path, workflow_dir, &id);
    let mention_target = mention_target(&root.path, &id).ok()?;
    Some(WorkflowSummary {
        id,
        command,
        title: spec.title,
        user_description: spec.user_description,
        search_terms: spec.search_terms,
        command_option_hints,
        root_label: root.label.clone(),
        root_kind: root.kind,
        root_path: root.path.clone(),
        path: workflow_dir.to_path_buf(),
        workflow_yaml_path,
        mention_target,
        validation,
        repair_mode,
    })
}

pub fn find_workflow_by_command<'a>(
    workflows: &'a [WorkflowSummary],
    command: &str,
) -> Option<&'a WorkflowSummary> {
    workflows
        .iter()
        .find(|workflow| workflow.command.as_deref() == Some(command))
}

fn normalize_workflow_command(command: Option<String>) -> Option<String> {
    let command = command?;
    let command = command.trim();
    if command.is_empty() || command.contains('/') || command.chars().any(char::is_whitespace) {
        return None;
    }
    Some(command.to_string())
}

fn default_workflow_command(id: &str) -> Option<String> {
    (!id.contains('/')).then_some(id.to_string())
}

fn relative_workflow_id(relative: &Path) -> Option<String> {
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        components.push(component.to_str()?.to_string());
    }
    Some(components.join("/"))
}

fn should_skip_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "target")
    )
}

fn package_dependency_names(package: &JsonValue, key: &str) -> Vec<String> {
    let mut names = package
        .get(key)
        .and_then(JsonValue::as_object)
        .map(|dependencies| dependencies.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    names.sort();
    names
}

fn git_status_lines(path: &Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn default_workflow_root(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> WorkflowRoot {
    let default_location = config.default_location.unwrap_or_default();
    workflow_roots(codex_home, cwd, config)
        .into_iter()
        .find(|root| {
            matches!(
                (default_location, root.kind),
                (WorkflowDefaultLocation::Global, WorkflowRootKind::Global)
                    | (WorkflowDefaultLocation::Project, WorkflowRootKind::Project)
            )
        })
        .unwrap_or_else(|| WorkflowRoot {
            kind: WorkflowRootKind::Global,
            label: "global".to_string(),
            path: AbsolutePathBuf::resolve_path_against_base("workflows", codex_home).to_path_buf(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::WorkflowHookKind;
    use crate::spec::WorkflowToolSpec;
    use crate::spec::write_workflow_spec;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[test]
    fn duplicate_ids_are_reported_across_roots() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let config = WorkflowsConfigToml::default();
        let global = home.path().join("workflows").join("reports").join("jira");
        let local = project.path().join(".codex/workflows/reports/jira");
        create_minimal_workflow(&global, "reports/jira", None);
        create_minimal_workflow(&local, "reports/jira", None);

        let err = find_workflow(home.path(), project.path(), &config, "reports/jira").unwrap_err();
        assert!(matches!(err, WorkflowRegistryError::Duplicate(_)));
    }

    #[test]
    fn workflow_discovery_uses_relative_path_id() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let workflow = home.path().join("workflows/reports/jira-summary");
        create_minimal_workflow(&workflow, "reports/jira-summary", None);

        let discovered =
            discover_workflows(home.path(), project.path(), &WorkflowsConfigToml::default())
                .unwrap();
        assert_eq!(discovered[0].id, "reports/jira-summary");
        assert_eq!(discovered[0].command, None);
    }

    #[test]
    fn workflow_tools_published_for_after_agent_hook() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let workflow = home.path().join("workflows/reports/jira-summary");
        create_minimal_workflow(
            &workflow,
            "reports/jira-summary",
            Some(WorkflowToolSpec {
                description: "Run the Jira summary workflow".to_string(),
                input_schema: serde_json::json!({ "type": "object" }),
                output_schema: serde_json::Value::Null,
                register_on: vec![WorkflowHookKind::AfterAgent],
            }),
        );

        let tools =
            discover_workflow_tools(home.path(), project.path(), &WorkflowsConfigToml::default())
                .unwrap();

        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].tool_name(),
            workflow_tool_name("reports/jira-summary")
        );
        assert_eq!(tools[0].tool.description, "Run the Jira summary workflow");
    }

    #[test]
    fn validate_workflow_dir_reports_layout_violations() {
        let root = TempDir::new().unwrap();
        let workflow = root.path().join("reports/jira-summary");
        fs::create_dir_all(workflow.join("src/tests")).unwrap();
        fs::create_dir_all(workflow.join("state")).unwrap();
        fs::create_dir_all(workflow.join("src")).unwrap();
        fs::create_dir_all(workflow.join("tests")).unwrap();
        fs::create_dir_all(workflow.join(".git")).unwrap();
        fs::write(
            workflow.join("README.md"),
            "# Test\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        )
        .unwrap();
        fs::write(
            workflow.join("DESIGN.md"),
            "# Test Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        )
        .unwrap();
        fs::write(
            workflow.join("package.json"),
            r#"{
  "name": "codex-workflow-test",
  "private": true,
  "type": "module",
  "dependencies": {
    "@openai/codex-sdk": "latest"
  }
}
"#,
        )
        .unwrap();
        fs::write(
            workflow.join("src/workflow.ts"),
            "import { defineWorkflow } from \"@openai/codex-sdk/workflow\";\n\nexport default defineWorkflow({ id: \"reports/jira-summary\", title: \"Test\", description: \"Test\", async run() { return { ok: true }; } });\n",
        )
        .unwrap();
        fs::write(
            workflow.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        fs::write(workflow.join("tests/workflow.test.ts"), "export {};\n").unwrap();
        fs::write(workflow.join("cache.db"), "db").unwrap();
        write_workflow_spec(
            &workflow.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: "reports/jira-summary".to_string(),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
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

        let validation = validate_workflow_dir(root.path(), &workflow, "reports/jira-summary");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert_eq!(
            validation.messages,
            vec![
                "test files must live under src/tests/: tests/workflow.test.ts".to_string(),
                "database files must live under state/: cache.db".to_string(),
            ]
        );
    }

    fn create_minimal_workflow(dir: &Path, id: &str, tool: Option<WorkflowToolSpec>) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("src/tests")).unwrap();
        fs::create_dir_all(dir.join("state")).unwrap();
        fs::write(
            dir.join("README.md"),
            "# Test\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        )
        .unwrap();
        fs::write(
            dir.join("DESIGN.md"),
            "# Test Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        )
        .unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{
  "name": "codex-workflow-test",
  "private": true,
  "type": "module",
  "dependencies": {
    "@openai/codex-sdk": "latest"
  }
}
"#,
        )
        .unwrap();
        fs::write(
            dir.join("src/workflow.ts"),
            "import { defineWorkflow } from \"@openai/codex-sdk/workflow\";\n\nexport default defineWorkflow({ id: \"reports/jira-summary\", title: \"Test\", description: \"Test\", async run() { return { ok: true }; } });\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {};\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        fs::write(dir.join("state/.gitkeep"), "").unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        write_workflow_spec(
            &dir.join(WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: id.to_string(),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "recovery": false,
                    }
                }),
                tool,
                ..Default::default()
            },
        )
        .unwrap();
    }
}
