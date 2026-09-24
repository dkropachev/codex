use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde_yaml::Value;

use crate::manifest::MAX_WORKFLOW_YAML_BYTES;
use crate::manifest::read_bounded_utf8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowCommand {
    pub id: String,
    pub command: String,
    pub description: String,
    pub option_hints: Vec<WorkflowCommandOptionHint>,
    pub workflow_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowCommandOptionHint {
    pub display: String,
    pub description: Option<String>,
}

pub fn discover_workflow_commands(codex_home: &Path, cwd: &Path) -> Vec<WorkflowCommand> {
    let mut commands = BTreeMap::new();
    discover_in_root(&codex_home.join("workflows"), &mut commands);
    discover_in_root(&cwd.join(".codex").join("workflows"), &mut commands);
    commands.into_values().collect()
}

fn discover_in_root(root: &Path, commands: &mut BTreeMap<String, WorkflowCommand>) {
    discover_in_dir(root, root, commands);
}

fn discover_in_dir(root: &Path, dir: &Path, commands: &mut BTreeMap<String, WorkflowCommand>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let is_package_root = dir.join("workflow.yaml").is_file();
    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        if is_package_root && is_package_internal_directory(&entry.file_name()) {
            continue;
        }
        let workflow_dir = entry.path();
        if let Some(command) = load_command(root, &workflow_dir) {
            commands.insert(command.id.clone(), command);
        }
        discover_in_dir(root, &workflow_dir, commands);
    }
}

fn is_package_internal_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | "artifacts" | "node_modules" | "src" | "state")
    )
}

fn load_command(root: &Path, workflow_dir: &Path) -> Option<WorkflowCommand> {
    let workflow_yaml = workflow_dir.join("workflow.yaml");
    let metadata = fs::symlink_metadata(&workflow_yaml).ok()?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_WORKFLOW_YAML_BYTES
    {
        return None;
    }
    let contents = read_bounded_utf8(&workflow_yaml, MAX_WORKFLOW_YAML_BYTES).ok()?;
    let raw = serde_yaml::from_str::<Value>(&contents).ok()?;
    let mapping = raw.as_mapping()?;
    let fallback_id = workflow_dir
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?
        .join("/");
    let raw_id = yaml_string(mapping, "id").unwrap_or(fallback_id);
    let command =
        yaml_string(mapping, "callableName").or_else(|| yaml_string(mapping, "command"))?;
    let canonical = mapping.contains_key(Value::String("apiVersion".to_string()));
    let id = if canonical {
        crate::normalize_workflow_id(&raw_id).ok()?
    } else {
        normalize_legacy_workflow_id(&raw_id)?
    };
    if (canonical && !is_valid_callable_name(&command))
        || (!canonical && !is_valid_legacy_command(&command))
    {
        return None;
    }
    let description = yaml_string(mapping, "description")
        .or_else(|| yaml_string(mapping, "userDescription"))
        .or_else(|| yaml_string(mapping, "title"))
        .unwrap_or_else(|| "Workflow command".to_string());
    Some(WorkflowCommand {
        id,
        command,
        description,
        option_hints: Vec::new(),
        workflow_dir: workflow_dir.to_path_buf(),
    })
}

fn normalize_legacy_workflow_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\\') {
        return None;
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return None;
    }
    let mut components = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        components.push(component.to_str()?.to_string());
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn is_valid_legacy_command(command: &str) -> bool {
    !command.is_empty()
        && !command.starts_with('-')
        && command
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn yaml_string(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    mapping
        .get(Value::String(key.to_string()))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn is_valid_callable_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
