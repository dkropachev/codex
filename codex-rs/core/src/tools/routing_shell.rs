use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_deterministic::normalize;
use crate::tools::routing_tool::RouterArgs;
use crate::tools::routing_tool::RouterTarget;
use crate::tools::routing_tool::RouterVerbosity;
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
    insert_wait_fields(&mut object, args);
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
    insert_i64(object, "max_output_tokens", max_output_tokens(args));
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

fn insert_wait_fields(object: &mut Map<String, Value>, args: &RouterArgs) {
    let kind = normalize(&args.action.kind);
    let wait_until_exit = args.action.wait_until_exit.unwrap_or(kind == "exec_wait");
    if wait_until_exit {
        object.insert("wait_until_exit".to_string(), Value::Bool(true));
        insert_i64(
            object,
            "wait_timeout_ms",
            args.action.wait_timeout_ms.or(args.action.timeout_ms),
        );
    }
}

fn max_output_tokens(args: &RouterArgs) -> Option<i64> {
    args.action.max_output_tokens.or(match args.verbosity {
        RouterVerbosity::Brief => Some(4_000),
        RouterVerbosity::Auto | RouterVerbosity::Normal | RouterVerbosity::Full => None,
    })
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
    if kind == "batch" {
        return batch_command(args);
    }
    if matches!(where_kind.as_str(), "filesystem" | "workspace")
        && matches!(
            kind.as_str(),
            "inspect" | "read_context" | "read_many" | "grep" | "find"
        )
    {
        return workspace_inspect_command(args);
    }
    if where_kind == "git" && matches!(kind.as_str(), "git_snapshot" | "snapshot") {
        return Ok(git_snapshot_command());
    }
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

fn batch_command(args: &RouterArgs) -> Result<String, FunctionCallError> {
    let commands = args.action.commands.clone().unwrap_or_default();
    let paths = all_paths(args);
    if commands.is_empty() && paths.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "tool_router batch route requires action.commands or path targets".to_string(),
        ));
    }

    let mut script = String::new();
    for (index, command) in commands.iter().enumerate() {
        push_command_section(&mut script, &format!("command {}", index + 1), command);
    }
    push_read_path_sections(&mut script, args, &paths);
    script.push_str("exit 0\n");
    Ok(script)
}

fn workspace_inspect_command(args: &RouterArgs) -> Result<String, FunctionCallError> {
    let paths = all_paths(args);
    let query = args.action.query.as_deref();
    if query.is_none() && paths.is_empty() {
        return Ok("rg --files | sed -n '1,200p'".to_string());
    }

    let mut script = String::new();
    if let Some(query) = query {
        let command = if paths.is_empty() {
            format!("rg -n -- {}", shell_quote(query))
        } else {
            format!(
                "rg -n -- {} {}",
                shell_quote(query),
                paths
                    .iter()
                    .map(|path| shell_quote(path))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        push_command_section(&mut script, "search", &command);
    }
    push_read_path_sections(&mut script, args, &paths);
    script.push_str("exit 0\n");
    Ok(script)
}

fn git_snapshot_command() -> String {
    let mut script = String::new();
    for (label, command) in [
        ("branch", "git branch --show-current"),
        (
            "upstream",
            "git rev-parse --abbrev-ref --symbolic-full-name @{u} 2>/dev/null || true",
        ),
        ("status", "git status --short --branch"),
        ("diff-stat", "git diff --stat"),
        ("staged-diff-stat", "git diff --cached --stat"),
        ("last-commit", "git log -1 --oneline"),
    ] {
        push_command_section(&mut script, label, command);
    }
    script.push_str("exit 0\n");
    script
}

fn push_read_path_sections(script: &mut String, args: &RouterArgs, paths: &[String]) {
    let start = args.action.offset.unwrap_or(1).max(1);
    let limit = args.action.limit.unwrap_or(200).max(1);
    let end = start.saturating_add(limit).saturating_sub(1);
    for path in paths {
        let quoted = shell_quote(path);
        let command = format!(
            "if [ -d {quoted} ]; then find {quoted} -maxdepth 2 -type f | sort | sed -n '1,200p'; elif [ -f {quoted} ]; then sed -n '{start},{end}p' {quoted}; else printf '%s\\n' {} >&2; fi",
            shell_quote(&format!("missing path: {path}"))
        );
        push_command_section(script, &format!("path {path}"), &command);
    }
}

fn push_command_section(script: &mut String, label: &str, command: &str) {
    script.push_str(&format!(
        "printf '%s\\n' {}\n",
        shell_quote(&format!("## {label}"))
    ));
    script.push_str("(\n");
    script.push_str(command);
    script.push_str("\n)\n");
    script.push_str("status=$?\n");
    script.push_str(&format!(
        "printf '## exit %s: %s\\n' {} \"$status\"\n",
        shell_quote(label)
    ));
}

fn all_paths(args: &RouterArgs) -> Vec<String> {
    let mut paths = Vec::new();
    push_path(&mut paths, args.action.path.clone());
    push_path(&mut paths, args.action.dir_path.clone());
    if let Some(action_paths) = args.action.paths.as_ref() {
        for path in action_paths {
            push_path(&mut paths, Some(path.clone()));
        }
    }
    if let Some(action_targets) = args.action.targets.as_ref() {
        for path in action_targets {
            push_path(&mut paths, Some(path.clone()));
        }
    }
    for target in &args.targets {
        push_path(&mut paths, target_path(target));
    }
    paths
}

fn push_path(paths: &mut Vec<String>, path: Option<String>) {
    if let Some(path) = path
        && !path.is_empty()
        && !paths.contains(&path)
    {
        paths.push(path);
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
    targets.iter().find_map(target_path)
}

fn target_path(target: &RouterTarget) -> Option<String> {
    target.path.clone().or_else(|| {
        matches!(
            target.kind.as_deref().map(normalize).as_deref(),
            Some("path") | Some("dir") | Some("directory")
        )
        .then(|| target.value.clone())
        .flatten()
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

    #[test]
    fn exec_wait_sets_wait_until_exit_arguments() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "run test",
            "where": {"kind": "shell"},
            "targets": [],
            "action": {
                "kind": "exec_wait",
                "cmd": "cargo test -p codex-core",
                "timeout_ms": 120000
            }
        }))
        .expect("router args");

        assert_eq!(
            exec_command_arguments(&args).expect("args"),
            json!({
                "cmd": "cargo test -p codex-core",
                "timeout_ms": 120000,
                "wait_until_exit": true,
                "wait_timeout_ms": 120000,
            })
        );
    }

    #[test]
    fn batch_command_labels_each_command() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "read context",
            "where": {"kind": "shell"},
            "targets": [],
            "action": {
                "kind": "batch",
                "commands": ["rg -n foo src", "git status --short"]
            }
        }))
        .expect("router args");

        let command = generated_shell_command(&args).expect("command");
        assert!(command.contains("## command 1"));
        assert!(command.contains("rg -n foo src"));
        assert!(command.contains("## exit %s: %s"));
    }

    #[test]
    fn git_snapshot_command_has_specific_sections() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "git snapshot",
            "where": {"kind": "git"},
            "targets": [],
            "action": {"kind": "git_snapshot"}
        }))
        .expect("router args");

        let command = generated_shell_command(&args).expect("command");
        assert!(command.contains("git status --short --branch"));
        assert!(command.contains("git diff --cached --stat"));
        assert!(command.contains("git log -1 --oneline"));
    }
}
