use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_deterministic::normalize;
use crate::tools::routing_tool::RouterArgs;
use crate::tools::routing_tool::RouterTarget;
use codex_protocol::models::ShellToolCallParams;
use codex_tools::ToolName;
use serde_json::Map;
use serde_json::Number;
use serde_json::Value;
use serde_json::json;

pub(super) fn call_for_local_shell(
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    Ok(ToolCall {
        tool_name: ToolName::plain("local_shell"),
        call_id,
        payload: ToolPayload::LocalShell {
            params: ShellToolCallParams {
                command: shell_command_vector(args)?,
                workdir: args.action.workdir.clone(),
                timeout_ms: args
                    .action
                    .timeout_ms
                    .and_then(|value| u64::try_from(value).ok()),
                sandbox_permissions: Some(SandboxPermissions::UseDefault),
                additional_permissions: None,
                prefix_rule: args.action.prefix_rule.clone(),
                justification: args.action.justification.clone(),
            },
        },
    })
}

pub(super) fn call_for_shell_like(
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
) -> Result<Option<ToolCall>, FunctionCallError> {
    for name in ["exec_command", "shell_command", "shell", "local_shell"] {
        let tool_name = ToolName::plain(name);
        if !index.has_handler(&tool_name) {
            continue;
        }
        let payload = match name {
            "exec_command" => ToolPayload::Function {
                arguments: exec_command_arguments(args)?.to_string(),
            },
            "shell_command" => ToolPayload::Function {
                arguments: shell_command_arguments(args)?.to_string(),
            },
            "shell" => ToolPayload::Function {
                arguments: shell_arguments(args)?.to_string(),
            },
            "local_shell" => return Ok(Some(call_for_local_shell(call_id, args)?)),
            "tool_router" | "write_stdin" | "apply_patch" | "tool_search" | "view_image" => {
                continue;
            }
            other => {
                return Err(FunctionCallError::Fatal(format!(
                    "unexpected shell-like tool candidate {other}"
                )));
            }
        };
        return Ok(Some(ToolCall {
            tool_name,
            call_id,
            payload,
        }));
    }
    Ok(None)
}

pub(super) fn exec_command_arguments(args: &RouterArgs) -> Result<Value, FunctionCallError> {
    let cmd = command_string(args)?;
    let mut object = Map::from_iter([("cmd".to_string(), Value::String(cmd))]);
    insert_common_shell_fields(&mut object, args);
    Ok(Value::Object(object))
}

pub(super) fn shell_command_arguments(args: &RouterArgs) -> Result<Value, FunctionCallError> {
    let command = command_string(args)?;
    let mut object = Map::from_iter([("command".to_string(), Value::String(command))]);
    insert_common_shell_fields(&mut object, args);
    Ok(Value::Object(object))
}

pub(super) fn shell_arguments(args: &RouterArgs) -> Result<Value, FunctionCallError> {
    let mut object = Map::from_iter([("command".to_string(), json!(shell_command_vector(args)?))]);
    insert_common_shell_fields(&mut object, args);
    Ok(Value::Object(object))
}

fn insert_common_shell_fields(object: &mut Map<String, Value>, args: &RouterArgs) {
    insert_string(object, "workdir", args.action.workdir.as_ref());
    insert_i64(object, "timeout_ms", args.action.timeout_ms);
    insert_i64(object, "yield_time_ms", args.action.yield_time_ms);
    insert_i64(object, "max_output_tokens", args.action.max_output_tokens);
    insert_string(
        object,
        "sandbox_permissions",
        args.action.sandbox_permissions.as_ref(),
    );
    insert_string(object, "justification", args.action.justification.as_ref());
    if let Some(prefix_rule) = args.action.prefix_rule.as_ref() {
        object.insert("prefix_rule".to_string(), json!(prefix_rule));
    }
}

fn command_string(args: &RouterArgs) -> Result<String, FunctionCallError> {
    if let Some(cmd) = args.action.cmd.as_ref() {
        return Ok(cmd.clone());
    }
    match args.action.command.as_ref() {
        Some(Value::String(command)) => Ok(command.clone()),
        Some(Value::Array(command)) => {
            let parts = command
                .iter()
                .map(|value| match value {
                    Value::String(part) => Ok(part.clone()),
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::Array(_)
                    | Value::Object(_) => Err(FunctionCallError::RespondToModel(
                        "tool_router action.command array must contain only strings".to_string(),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(codex_shell_command::parse_command::shlex_join(&parts))
        }
        Some(Value::Null)
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::Object(_)) => Err(FunctionCallError::RespondToModel(
            "tool_router shell route requires action.cmd or string/array action.command"
                .to_string(),
        )),
        None => generated_shell_command(args),
    }
}

fn shell_command_vector(args: &RouterArgs) -> Result<Vec<String>, FunctionCallError> {
    match args.action.command.as_ref() {
        Some(Value::Array(command)) => command
            .iter()
            .map(|value| match value {
                Value::String(part) => Ok(part.clone()),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Err(FunctionCallError::RespondToModel(
                    "tool_router action.command array must contain only strings".to_string(),
                )),
            })
            .collect(),
        Some(Value::String(command)) => Ok(shell_wrapper_command(command)),
        Some(Value::Null)
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::Object(_)) => Err(FunctionCallError::RespondToModel(
            "tool_router shell route requires action.cmd or string/array action.command"
                .to_string(),
        )),
        None => Ok(shell_wrapper_command(&command_string(args)?)),
    }
}

fn generated_shell_command(args: &RouterArgs) -> Result<String, FunctionCallError> {
    let kind = normalize(&args.action.kind);
    let where_kind = normalize(&args.where_.kind);
    let path = args
        .action
        .path
        .clone()
        .or_else(|| args.action.dir_path.clone())
        .or_else(|| first_target_path(&args.targets));
    match (where_kind.as_str(), kind.as_str(), path) {
        ("filesystem" | "workspace", "read", Some(path)) => {
            Ok(format!("sed -n '1,200p' {}", shell_quote(&path)))
        }
        ("filesystem" | "workspace", "list", Some(path)) => {
            Ok(format!("ls -la {}", shell_quote(&path)))
        }
        ("git", "status", None) => Ok("git status --short".to_string()),
        ("git", "diff", None) => Ok("git diff --stat && git diff".to_string()),
        ("git", "log", None) => Ok("git log --oneline -20".to_string()),
        ("shell" | "git" | "filesystem" | "workspace", _, None)
        | (_, "exec" | "shell" | "command" | "git" | "read" | "grep" | "find", None)
        | (_, _, Some(_)) => Err(FunctionCallError::RespondToModel(
            "tool_router shell route requires action.cmd for this request".to_string(),
        )),
        (
            "none" | "mcp" | "app" | "skill" | "web" | "image" | "agent" | "memory" | "config",
            _,
            None,
        )
        | (_, _, None) => Err(FunctionCallError::RespondToModel(
            "tool_router shell route requires action.cmd".to_string(),
        )),
    }
}

fn shell_wrapper_command(command: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![
            "powershell.exe".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ]
    } else {
        vec!["bash".to_string(), "-lc".to_string(), command.to_string()]
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn first_target_path(targets: &[RouterTarget]) -> Option<String> {
    targets.iter().find_map(|target| {
        target.path.clone().or_else(|| {
            (target.kind.as_deref().map(normalize).as_deref() == Some("path"))
                .then(|| target.value.clone())
                .flatten()
        })
    })
}

fn insert_string(object: &mut Map<String, Value>, key: &str, value: Option<&String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.clone()));
    }
}

fn insert_i64(object: &mut Map<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::Number(Number::from(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn generated_shell_read_command_quotes_path() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "read file",
            "where": {"kind": "filesystem"},
            "targets": [{"kind": "path", "path": "/tmp/a b's.txt"}],
            "action": {"kind": "read", "description": "read file"},
            "verbosity": "auto"
        }))
        .expect("router args");

        assert_eq!(
            generated_shell_command(&args).expect("command"),
            "sed -n '1,200p' '/tmp/a b'\\''s.txt'"
        );
    }
}
