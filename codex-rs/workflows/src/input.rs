use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;

use serde_json::Map;
use serde_json::Value;

use crate::WorkflowCommand;

const MAX_INPUT_FILE_BYTES: u64 = 1024 * 1024;
type ParsedWorkflowArguments = (Map<String, Value>, Map<String, Value>);

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

impl fmt::Display for WorkflowInvocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowInvocationError {}

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

#[deprecated(note = "dispatch WorkflowInvocation through the shared workflow runner")]
pub fn build_workflow_shell_command(
    command: &WorkflowCommand,
    cwd: &Path,
    args: &str,
) -> Result<String, WorkflowInvocationError> {
    let invocation = build_workflow_invocation(command, cwd, args)?;
    let input = serde_json::to_string(&invocation.input)
        .map_err(|err| WorkflowInvocationError::new(format!("failed to serialize input: {err}")))?;
    shlex::try_join([
        "codex",
        "workflow",
        "run",
        command.id.as_str(),
        "--input",
        input.as_str(),
    ])
    .map_err(|err| WorkflowInvocationError::new(format!("failed to quote command: {err}")))
}

pub fn workflow_invocation_input(cwd: &Path, args: &str) -> Result<Value, WorkflowInvocationError> {
    let args = shlex::split(args).ok_or_else(|| {
        WorkflowInvocationError::new("Invalid workflow arguments: unmatched quote.")
    })?;
    workflow_invocation_input_from_args(cwd, &args)
}

pub fn workflow_invocation_input_from_args(
    cwd: &Path,
    args: &[String],
) -> Result<Value, WorkflowInvocationError> {
    let (base, flags) = parse_args(cwd, args)?;
    normalize_workflow_input(cwd, Value::Object(base), flags)
}

pub fn normalize_workflow_input(
    cwd: &Path,
    input: Value,
    flags: Map<String, Value>,
) -> Result<Value, WorkflowInvocationError> {
    normalize_workflow_input_with_working_directory(&cwd.to_string_lossy(), input, flags)
}

pub fn normalize_workflow_input_with_working_directory(
    working_directory: &str,
    input: Value,
    flags: Map<String, Value>,
) -> Result<Value, WorkflowInvocationError> {
    let Value::Object(mut input) = input else {
        return Err(WorkflowInvocationError::new(
            "Invalid workflow arguments: workflow input must be a JSON object.",
        ));
    };
    input.extend(flags);
    input
        .entry("workingDirectory".to_string())
        .or_insert_with(|| Value::String(working_directory.to_string()));
    Ok(Value::Object(input))
}

fn parse_args(
    cwd: &Path,
    args: &[String],
) -> Result<ParsedWorkflowArguments, WorkflowInvocationError> {
    let mut base = Map::new();
    let mut flag_values = BTreeMap::<String, Vec<Value>>::new();
    let mut input_seen = false;
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        let input_value = if token == "--input" {
            index += 1;
            Some(
                args.get(index)
                    .ok_or_else(|| {
                        WorkflowInvocationError::new(
                            "Invalid workflow arguments: expected a value after --input.",
                        )
                    })?
                    .as_str(),
            )
        } else {
            token.strip_prefix("--input=")
        };
        if let Some(value) = input_value {
            if input_seen {
                return Err(WorkflowInvocationError::new(
                    "Invalid workflow arguments: --input can only be provided once.",
                ));
            }
            base = parse_input_object(cwd, value)?;
            input_seen = true;
        } else if let Some(flag) = token.strip_prefix("--") {
            let (raw_key, value) = if let Some((key, value)) = flag.split_once('=') {
                (key, value.to_string())
            } else if args
                .get(index + 1)
                .is_some_and(|next| !next.starts_with("--"))
            {
                index += 1;
                (flag, args[index].clone())
            } else {
                (flag, "true".to_string())
            };
            let key = kebab_to_camel_case(raw_key)?;
            flag_values
                .entry(key)
                .or_default()
                .push(parse_arg_value(&value));
        } else {
            return Err(WorkflowInvocationError::new(format!(
                "Invalid workflow arguments: positional value '{token}' is no longer supported; migrate to --input <JSON> or named --kebab-case flags."
            )));
        }
        index += 1;
    }
    let flags = flag_values
        .into_iter()
        .map(|(key, values)| {
            let value = if values.len() == 1 {
                values.into_iter().next().unwrap_or(Value::Null)
            } else {
                Value::Array(values)
            };
            (key, value)
        })
        .collect();
    Ok((base, flags))
}

fn parse_input_object(
    cwd: &Path,
    value: &str,
) -> Result<Map<String, Value>, WorkflowInvocationError> {
    let contents = if let Some(path) = value.strip_prefix('@') {
        if path.is_empty() {
            return Err(WorkflowInvocationError::new(
                "Invalid workflow arguments: --input @file requires a file path.",
            ));
        }
        let path = cwd.join(path);
        let file = fs::File::open(&path).map_err(|err| {
            WorkflowInvocationError::new(format!(
                "Invalid workflow arguments: failed to read input file {}: {err}",
                path.display()
            ))
        })?;
        let mut contents = String::new();
        file.take(MAX_INPUT_FILE_BYTES + 1)
            .read_to_string(&mut contents)
            .map_err(|err| {
                WorkflowInvocationError::new(format!(
                    "Invalid workflow arguments: failed to read input file {}: {err}",
                    path.display()
                ))
            })?;
        if contents.len() as u64 > MAX_INPUT_FILE_BYTES {
            return Err(WorkflowInvocationError::new(format!(
                "Invalid workflow arguments: input file exceeds {MAX_INPUT_FILE_BYTES} bytes."
            )));
        }
        contents
    } else {
        value.to_string()
    };
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(WorkflowInvocationError::new(
            "Invalid workflow arguments: --input must be a JSON object.",
        )),
        Err(err) => Err(WorkflowInvocationError::new(format!(
            "Invalid workflow arguments: --input is not valid JSON: {err}"
        ))),
    }
}

fn kebab_to_camel_case(raw_key: &str) -> Result<String, WorkflowInvocationError> {
    let mut parts = raw_key.split('-');
    let first = parts.next().unwrap_or_default();
    if !valid_flag_part(first) || first.as_bytes()[0].is_ascii_digit() {
        return Err(invalid_flag_name(raw_key));
    }
    let mut normalized = first.to_string();
    for part in parts {
        if !valid_flag_part(part) {
            return Err(invalid_flag_name(raw_key));
        }
        let mut chars = part.chars();
        let first = chars.next().ok_or_else(|| invalid_flag_name(raw_key))?;
        normalized.push(first.to_ascii_uppercase());
        normalized.push_str(chars.as_str());
    }
    Ok(normalized)
}

fn valid_flag_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
}

fn invalid_flag_name(raw_key: &str) -> WorkflowInvocationError {
    WorkflowInvocationError::new(format!(
        "Invalid workflow arguments: invalid kebab-case flag name '--{raw_key}'."
    ))
}

fn parse_arg_value(value: &str) -> Value {
    serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()))
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
