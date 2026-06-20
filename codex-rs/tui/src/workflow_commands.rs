use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde_json::Map;
use serde_json::Value;

const MAX_WORKFLOW_YAML_BYTES: u64 = 64 * 1024;

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

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowInvocation {
    pub workflow_dir: PathBuf,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowInvocationError {
    message: String,
}

impl WorkflowInvocationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub fn discover_workflow_commands(codex_home: &Path, cwd: &Path) -> Vec<WorkflowCommand> {
    let mut commands = BTreeMap::new();
    discover_workflow_commands_in_root(&codex_home.join("workflows"), &mut commands);
    discover_workflow_commands_in_root(&cwd.join(".codex").join("workflows"), &mut commands);
    commands.into_values().collect()
}

pub fn build_workflow_shell_command(
    command: &WorkflowCommand,
    cwd: &Path,
    args: &str,
) -> Result<String, WorkflowInvocationError> {
    let WorkflowInvocation {
        workflow_dir,
        input,
        ..
    } = build_workflow_invocation(command, cwd, args)?;
    let input_json = serde_json::to_string(&input)
        .map_err(|err| WorkflowInvocationError::new(format!("failed to serialize input: {err}")))?;
    let workflow_dir = workflow_dir.to_string_lossy();
    let quoted_workflow_dir = shlex::try_quote(&workflow_dir)
        .map_err(|err| WorkflowInvocationError::new(format!("failed to quote path: {err}")))?;
    let invocation =
        shlex::try_join(["bun", "src/workflow.ts", "--input", &input_json]).map_err(|err| {
            WorkflowInvocationError::new(format!("failed to quote workflow command: {err}"))
        })?;
    Ok(format!("cd {quoted_workflow_dir} && {invocation}"))
}

pub fn build_workflow_invocation(
    command: &WorkflowCommand,
    cwd: &Path,
    args: &str,
) -> Result<WorkflowInvocation, WorkflowInvocationError> {
    Ok(WorkflowInvocation {
        workflow_dir: command.workflow_dir.clone(),
        input: workflow_invocation_input(cwd, args)?,
    })
}

pub fn workflow_invocation_input(cwd: &Path, args: &str) -> Result<Value, WorkflowInvocationError> {
    let tokens = shlex::split(args).ok_or_else(|| {
        WorkflowInvocationError::new("Invalid workflow arguments: unmatched quote.")
    })?;
    workflow_invocation_input_from_args(cwd, &tokens)
}

pub fn workflow_invocation_input_from_args(
    cwd: &Path,
    args: &[String],
) -> Result<Value, WorkflowInvocationError> {
    let mut input = parse_workflow_args(args)?;
    input
        .entry("workingDirectory".to_string())
        .or_insert_with(|| Value::String(cwd.to_string_lossy().to_string()));
    Ok(Value::Object(input))
}

fn discover_workflow_commands_in_root(
    workflows_root: &Path,
    commands: &mut BTreeMap<String, WorkflowCommand>,
) {
    discover_workflow_commands_in_dir(workflows_root, workflows_root, commands);
}

fn discover_workflow_commands_in_dir(
    workflows_root: &Path,
    dir: &Path,
    commands: &mut BTreeMap<String, WorkflowCommand>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let workflow_dir = entry.path();
        if let Some(command) = load_workflow_command(workflows_root, &workflow_dir) {
            commands.insert(command.id.clone(), command);
        }
        discover_workflow_commands_in_dir(workflows_root, &workflow_dir, commands);
    }
}

fn load_workflow_command(workflows_root: &Path, workflow_dir: &Path) -> Option<WorkflowCommand> {
    let workflow_yaml = workflow_dir.join("workflow.yaml");
    let metadata = fs::metadata(&workflow_yaml).ok()?;
    if metadata.len() > MAX_WORKFLOW_YAML_BYTES {
        return None;
    }
    let contents = fs::read_to_string(workflow_yaml).ok()?;
    let fallback_id = workflow_dir
        .strip_prefix(workflows_root)
        .ok()
        .and_then(normalize_workflow_id_path)?;
    parse_workflow_metadata(&contents, workflow_dir, &fallback_id)
}

fn parse_workflow_metadata(
    contents: &str,
    workflow_dir: &Path,
    fallback_id: &str,
) -> Option<WorkflowCommand> {
    let mut id = None;
    let mut command = None;
    let mut title = None;
    let mut user_description = None;

    for line in contents.lines() {
        if line.chars().next().is_some_and(char::is_whitespace) {
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = parse_yaml_scalar(value.trim()) else {
            continue;
        };
        match key.trim() {
            "id" => id = Some(value),
            "command" => command = Some(value),
            "title" => title = Some(value),
            "userDescription" => user_description = Some(value),
            _ => {}
        }
    }

    let id = id.unwrap_or_else(|| fallback_id.to_string());
    normalize_workflow_id(&id)?;
    let command = command?;
    if !is_valid_workflow_command(&command) {
        return None;
    }
    let description = user_description
        .or(title)
        .unwrap_or_else(|| "Workflow command".to_string());
    let option_hints = parse_workflow_option_hints(contents);
    Some(WorkflowCommand {
        id,
        command,
        description,
        option_hints,
        workflow_dir: workflow_dir.to_path_buf(),
    })
}

fn parse_workflow_option_hints(contents: &str) -> Vec<WorkflowCommandOptionHint> {
    let mut hints = Vec::new();
    let mut in_usage = false;
    let mut in_options = false;
    let mut current = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len().saturating_sub(line.trim_start().len());
        if indent == 0 {
            push_option_hint(&mut hints, current.take());
            in_usage = top_level_key(trimmed) == Some("usage");
            in_options = false;
            continue;
        }
        if !in_usage {
            continue;
        }
        if indent == 2 && top_level_key(trimmed) == Some("options") {
            in_options = true;
            continue;
        }
        if !in_options || indent < 4 {
            continue;
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            push_option_hint(&mut hints, current.take());
            if let Some((key, value)) = item.split_once(':') {
                let mut builder = WorkflowOptionHintBuilder::default();
                builder.set(key.trim(), value.trim());
                current = Some(builder);
            } else if let Some(display) = parse_yaml_scalar(item) {
                hints.push(WorkflowCommandOptionHint {
                    display,
                    description: None,
                });
            }
            continue;
        }

        if let Some(builder) = current.as_mut()
            && let Some((key, value)) = trimmed.split_once(':')
        {
            builder.set(key.trim(), value.trim());
        }
    }

    push_option_hint(&mut hints, current);
    hints
}

fn top_level_key(line: &str) -> Option<&str> {
    line.split_once(':')
        .map(|(key, _)| key.trim())
        .filter(|key| !key.is_empty())
}

#[derive(Default)]
struct WorkflowOptionHintBuilder {
    display: Option<String>,
    flag: Option<String>,
    value_hint: Option<String>,
    description: Option<String>,
}

impl WorkflowOptionHintBuilder {
    fn set(&mut self, key: &str, value: &str) {
        let Some(value) = parse_yaml_scalar(value) else {
            return;
        };
        match key {
            "display" => self.display = Some(value),
            "flag" | "name" => self.flag = Some(value),
            "valueHint" => self.value_hint = Some(value),
            "description" => self.description = Some(value),
            _ => {}
        }
    }

    fn build(self) -> Option<WorkflowCommandOptionHint> {
        let display = self.display.or_else(|| {
            self.flag.map(|flag| match self.value_hint {
                Some(value_hint) => format!("{flag} {value_hint}"),
                None => flag,
            })
        })?;
        Some(WorkflowCommandOptionHint {
            display,
            description: self.description,
        })
    }
}

fn push_option_hint(
    hints: &mut Vec<WorkflowCommandOptionHint>,
    builder: Option<WorkflowOptionHintBuilder>,
) {
    if let Some(hint) = builder.and_then(WorkflowOptionHintBuilder::build) {
        hints.push(hint);
    }
}

fn parse_yaml_scalar(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let value = strip_inline_comment(value).trim();
    if value.is_empty() {
        return None;
    }
    if let Some(quoted) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        serde_json::from_str::<String>(value)
            .ok()
            .or_else(|| Some(quoted.to_string()))
    } else if let Some(quoted) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        Some(quoted.replace("''", "'"))
    } else {
        Some(value.to_string())
    }
}

fn strip_inline_comment(value: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (idx, ch) in value.char_indices() {
        if in_double && escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single
                && !in_double
                && value[..idx]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace) =>
            {
                return &value[..idx];
            }
            _ => {}
        }
    }
    value
}

fn is_valid_workflow_command(command: &str) -> bool {
    !command.is_empty()
        && !command.starts_with('-')
        && command
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn normalize_workflow_id_path(path: &Path) -> Option<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            return None;
        };
        components.push(component.to_str()?.to_string());
    }
    let id = components.join("/");
    normalize_workflow_id(&id)
}

fn normalize_workflow_id(raw: &str) -> Option<String> {
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
        match component {
            std::path::Component::Normal(component) => {
                components.push(component.to_str()?.to_string());
            }
            std::path::Component::CurDir | std::path::Component::ParentDir => return None,
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return None,
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn parse_workflow_args(tokens: &[String]) -> Result<Map<String, Value>, WorkflowInvocationError> {
    let mut input = Map::new();
    let mut input_seen = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token == "--input" {
            if input_seen {
                return Err(WorkflowInvocationError::new(
                    "Invalid workflow arguments: --input can only be provided once.",
                ));
            }
            index += 1;
            let Some(value) = tokens.get(index) else {
                return Err(WorkflowInvocationError::new(
                    "Invalid workflow arguments: expected a value after --input.",
                ));
            };
            input = parse_input_object(value)?;
            input_seen = true;
        } else if let Some(value) = token.strip_prefix("--input=") {
            if input_seen {
                return Err(WorkflowInvocationError::new(
                    "Invalid workflow arguments: --input can only be provided once.",
                ));
            }
            input = parse_input_object(value)?;
            input_seen = true;
        } else if let Some(flag) = token.strip_prefix("--") {
            let (raw_key, value) = if let Some((key, value)) = flag.split_once('=') {
                (key, value.to_string())
            } else if tokens
                .get(index + 1)
                .is_some_and(|next| !next.starts_with("--"))
            {
                index += 1;
                (flag, tokens[index].clone())
            } else {
                (flag, "true".to_string())
            };
            let key = normalize_flag_key(raw_key)?;
            insert_arg(&mut input, key, parse_arg_value(&value));
        } else {
            return Err(WorkflowInvocationError::new(format!(
                "Invalid workflow arguments: unexpected positional argument '{token}'. Use --name value pairs."
            )));
        }
        index += 1;
    }
    Ok(input)
}

fn parse_input_object(value: &str) -> Result<Map<String, Value>, WorkflowInvocationError> {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(WorkflowInvocationError::new(
            "Invalid workflow arguments: --input must be a JSON object.",
        )),
        Err(err) => Err(WorkflowInvocationError::new(format!(
            "Invalid workflow arguments: --input is not valid JSON: {err}"
        ))),
    }
}

fn normalize_flag_key(raw_key: &str) -> Result<String, WorkflowInvocationError> {
    let mut normalized = String::new();
    let mut uppercase_next = false;
    for ch in raw_key.chars() {
        if matches!(ch, '-' | '_') {
            if normalized.is_empty() {
                return Err(invalid_flag_name(raw_key));
            }
            uppercase_next = true;
            continue;
        }
        if !ch.is_ascii_alphanumeric() {
            return Err(invalid_flag_name(raw_key));
        }
        if normalized.is_empty() && ch.is_ascii_digit() {
            return Err(invalid_flag_name(raw_key));
        }
        if uppercase_next {
            normalized.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            normalized.push(ch);
        }
    }
    if normalized.is_empty() || uppercase_next {
        return Err(invalid_flag_name(raw_key));
    }
    Ok(normalized)
}

fn invalid_flag_name(raw_key: &str) -> WorkflowInvocationError {
    WorkflowInvocationError::new(format!(
        "Invalid workflow arguments: invalid flag name '--{raw_key}'."
    ))
}

fn parse_arg_value(value: &str) -> Value {
    serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

fn insert_arg(input: &mut Map<String, Value>, key: String, value: Value) {
    if let Some(existing) = input.remove(&key) {
        let value = match existing {
            Value::Array(mut values) => {
                values.push(value);
                Value::Array(values)
            }
            existing => Value::Array(vec![existing, value]),
        };
        input.insert(key, value);
    } else {
        input.insert(key, value);
    }
}

#[cfg(test)]
#[path = "workflow_commands_tests.rs"]
mod tests;
