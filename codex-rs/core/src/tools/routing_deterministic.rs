use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::tools::context::ToolPayload;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_shell;
use crate::tools::routing_tool::RouterArgs;
use crate::tools::routing_tool::RouterTarget;
use codex_protocol::models::SearchToolCallParams;
use codex_tools::ToolName;
use serde_json::Map;
use serde_json::Number;
use serde_json::Value;
use serde_json::json;

pub(super) fn exact_tool_name(
    args: &RouterArgs,
    index: &ToolRouterIndex,
) -> Result<Option<ToolName>, FunctionCallError> {
    let namespace = args.where_.namespace.as_deref().or_else(|| {
        args.targets
            .iter()
            .find_map(|target| target.namespace.as_deref())
    });
    for candidate in exact_tool_candidates(args) {
        if let Some(tool_name) = index.find_exact(&candidate, namespace)? {
            return Ok(Some(tool_name));
        }
    }
    Ok(None)
}

fn exact_tool_candidates(args: &RouterArgs) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(tool) = args.action.tool.as_ref() {
        candidates.push(tool.clone());
    }
    if matches!(
        normalize(&args.action.kind).as_str(),
        "direct_tool" | "tool" | "function"
    ) && let Some(name) = args.action.name.as_ref()
    {
        candidates.push(name.clone());
    }
    for target in &args.targets {
        if target.kind.as_deref().map(normalize).as_deref() == Some("tool") {
            for value in [
                target.name.as_ref(),
                target.id.as_ref(),
                target.value.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                candidates.push(value.clone());
            }
        }
    }
    candidates.push(args.action.kind.clone());
    candidates
}

pub(super) async fn call_for_exact_tool(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    tool_name: ToolName,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    if let Some(tool_info) = session.resolve_mcp_tool_info(&tool_name).await {
        return Ok(ToolCall {
            tool_name: tool_info.canonical_tool_name(),
            call_id,
            payload: ToolPayload::Mcp {
                server: tool_info.server_name,
                tool: tool_info.tool.name.to_string(),
                raw_arguments: action_arguments_json(args, ArgumentMode::Mcp)?,
            },
        });
    }

    if tool_name.name == "tool_search" && tool_name.namespace.is_none() {
        return call_for_tool_search(call_id, args);
    }

    if tool_name.name == "exec_command" && tool_name.namespace.is_none() {
        return Ok(ToolCall {
            tool_name,
            call_id,
            payload: ToolPayload::Function {
                arguments: routing_shell::exec_command_arguments(args)?.to_string(),
            },
        });
    }

    if tool_name.name == "shell_command" && tool_name.namespace.is_none() {
        return Ok(ToolCall {
            tool_name,
            call_id,
            payload: ToolPayload::Function {
                arguments: routing_shell::shell_command_arguments(args)?.to_string(),
            },
        });
    }

    if tool_name.name == "shell" && tool_name.namespace.is_none() {
        return Ok(ToolCall {
            tool_name,
            call_id,
            payload: ToolPayload::Function {
                arguments: routing_shell::shell_arguments(args)?.to_string(),
            },
        });
    }

    if tool_name.name == "local_shell" && tool_name.namespace.is_none() {
        return routing_shell::call_for_local_shell(call_id, args);
    }

    let payload = if index.is_freeform(&tool_name) {
        ToolPayload::Custom {
            input: action_text_input(args).ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "tool_router route requires action.patch or string action.input for the freeform tool"
                        .to_string(),
                )
            })?,
        }
    } else {
        ToolPayload::Function {
            arguments: action_arguments_json(args, ArgumentMode::Function)?,
        }
    };
    Ok(ToolCall {
        tool_name,
        call_id,
        payload,
    })
}

pub(super) fn call_for_apply_patch(
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let tool_name = ToolName::plain("apply_patch");
    let patch = action_text_input(args).ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "tool_router apply_patch route requires action.patch or string action.input"
                .to_string(),
        )
    })?;
    let payload = if index.is_freeform(&tool_name) {
        ToolPayload::Custom { input: patch }
    } else {
        ToolPayload::Function {
            arguments: json!({ "input": patch }).to_string(),
        }
    };
    Ok(ToolCall {
        tool_name,
        call_id,
        payload,
    })
}

pub(super) fn call_for_write_stdin(
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let session_id = args.action.session_id.ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "tool_router write_stdin route requires action.session_id".to_string(),
        )
    })?;
    let mut object = Map::new();
    object.insert(
        "session_id".to_string(),
        Value::Number(Number::from(session_id)),
    );
    object.insert(
        "chars".to_string(),
        Value::String(args.action.chars.clone().unwrap_or_default()),
    );
    insert_i64(&mut object, "yield_time_ms", args.action.yield_time_ms);
    insert_i64(
        &mut object,
        "max_output_tokens",
        args.action.max_output_tokens,
    );
    Ok(ToolCall {
        tool_name: ToolName::plain("write_stdin"),
        call_id,
        payload: ToolPayload::Function {
            arguments: Value::Object(object).to_string(),
        },
    })
}

pub(super) fn call_for_tool_search(
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let query = args
        .action
        .query
        .clone()
        .or_else(|| first_target_value(&args.targets, "query"))
        .or_else(|| Some(args.request.clone()))
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "tool_router tool_search route requires action.query".to_string(),
            )
        })?;
    let limit = args
        .action
        .limit
        .and_then(|value| usize::try_from(value).ok());
    Ok(ToolCall {
        tool_name: ToolName::plain("tool_search"),
        call_id,
        payload: ToolPayload::ToolSearch {
            arguments: SearchToolCallParams { query, limit },
        },
    })
}

pub(super) fn fanout_for_tool_search(call_id: &str, args: &RouterArgs) -> Option<Vec<ToolCall>> {
    let queries = target_values(&args.targets, "query");
    if queries.len() < 2 {
        return None;
    }

    let limit = args
        .action
        .limit
        .and_then(|value| usize::try_from(value).ok());
    Some(
        queries
            .into_iter()
            .enumerate()
            .map(|(index, query)| ToolCall {
                tool_name: ToolName::plain("tool_search"),
                call_id: fanout_call_id(call_id, index),
                payload: ToolPayload::ToolSearch {
                    arguments: SearchToolCallParams { query, limit },
                },
            })
            .collect(),
    )
}

pub(super) fn call_for_agent_tool(
    call_id: String,
    tool_name: ToolName,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let mut object = input_object(args).unwrap_or_default();
    if tool_name.name == "spawn_agent" && !object.contains_key("message") {
        let message = args
            .action
            .agent_task
            .clone()
            .or_else(|| string_input(&args.action.input))
            .unwrap_or_else(|| args.request.clone());
        object.insert("message".to_string(), Value::String(message));
    }
    if !object.contains_key("target")
        && let Some(target) = args
            .action
            .target
            .clone()
            .or_else(|| first_agent_target(args))
    {
        object.insert("target".to_string(), Value::String(target));
    }
    if !object.contains_key("targets")
        && let Some(targets) = args.action.targets.as_ref()
    {
        object.insert("targets".to_string(), json!(targets));
    }
    insert_i64(&mut object, "timeout_ms", args.action.timeout_ms);
    Ok(ToolCall {
        tool_name,
        call_id,
        payload: ToolPayload::Function {
            arguments: Value::Object(object).to_string(),
        },
    })
}

pub(super) fn call_for_view_image(
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let path = args
        .action
        .path
        .clone()
        .or_else(|| first_target_path(&args.targets))
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "tool_router view_image route requires a path target or action.path".to_string(),
            )
        })?;
    let mut object = Map::from_iter([("path".to_string(), Value::String(path))]);
    if let Some(detail) = args.action.detail.as_ref() {
        object.insert("detail".to_string(), Value::String(detail.clone()));
    }
    Ok(ToolCall {
        tool_name: ToolName::plain("view_image"),
        call_id,
        payload: ToolPayload::Function {
            arguments: Value::Object(object).to_string(),
        },
    })
}

pub(super) fn fanout_for_view_image(
    call_id: &str,
    args: &RouterArgs,
) -> Result<Option<Vec<ToolCall>>, FunctionCallError> {
    let paths = target_paths(&args.targets);
    if paths.len() < 2 {
        return Ok(None);
    }

    paths
        .into_iter()
        .enumerate()
        .map(|(index, path)| {
            let mut object = Map::from_iter([("path".to_string(), Value::String(path))]);
            if let Some(detail) = args.action.detail.as_ref() {
                object.insert("detail".to_string(), Value::String(detail.clone()));
            }
            Ok(ToolCall {
                tool_name: ToolName::plain("view_image"),
                call_id: fanout_call_id(call_id, index),
                payload: ToolPayload::Function {
                    arguments: Value::Object(object).to_string(),
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub(super) fn call_for_list_dir(
    call_id: String,
    args: &RouterArgs,
) -> Result<ToolCall, FunctionCallError> {
    let dir_path = args
        .action
        .dir_path
        .clone()
        .or_else(|| args.action.path.clone())
        .or_else(|| first_target_path(&args.targets))
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "tool_router list_dir route requires a path target or action.dir_path".to_string(),
            )
        })?;
    let mut object = Map::from_iter([("dir_path".to_string(), Value::String(dir_path))]);
    insert_i64(&mut object, "offset", args.action.offset);
    insert_i64(&mut object, "limit", args.action.limit);
    insert_i64(&mut object, "depth", args.action.depth);
    Ok(ToolCall {
        tool_name: ToolName::plain("list_dir"),
        call_id,
        payload: ToolPayload::Function {
            arguments: Value::Object(object).to_string(),
        },
    })
}

pub(super) fn fanout_for_list_dir(
    call_id: &str,
    args: &RouterArgs,
) -> Result<Option<Vec<ToolCall>>, FunctionCallError> {
    let paths = target_paths(&args.targets);
    if paths.len() < 2 {
        return Ok(None);
    }

    paths
        .into_iter()
        .enumerate()
        .map(|(index, dir_path)| {
            let mut object = Map::from_iter([("dir_path".to_string(), Value::String(dir_path))]);
            insert_i64(&mut object, "offset", args.action.offset);
            insert_i64(&mut object, "limit", args.action.limit);
            insert_i64(&mut object, "depth", args.action.depth);
            Ok(ToolCall {
                tool_name: ToolName::plain("list_dir"),
                call_id: fanout_call_id(call_id, index),
                payload: ToolPayload::Function {
                    arguments: Value::Object(object).to_string(),
                },
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub(super) fn fanout_call_id(call_id: &str, index: usize) -> String {
    format!("{call_id}:fanout:{index}")
}

fn action_arguments_json(
    args: &RouterArgs,
    mode: ArgumentMode,
) -> Result<String, FunctionCallError> {
    if let Some(object) = match mode {
        ArgumentMode::Function => input_object(args),
        ArgumentMode::Mcp => mcp_object(args),
    } {
        return Ok(Value::Object(object).to_string());
    }
    let object = action_payload_object(args);
    Ok(Value::Object(object).to_string())
}

#[derive(Clone, Copy)]
enum ArgumentMode {
    Function,
    Mcp,
}

fn input_object(args: &RouterArgs) -> Option<Map<String, Value>> {
    value_object(args.action.input.as_ref())
}

fn mcp_object(args: &RouterArgs) -> Option<Map<String, Value>> {
    value_object(args.action.mcp_args.as_ref()).or_else(|| input_object(args))
}

fn value_object(value: Option<&Value>) -> Option<Map<String, Value>> {
    match value {
        Some(Value::Object(object)) => Some(object.clone()),
        Some(Value::Null)
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::String(_))
        | Some(Value::Array(_))
        | None => None,
    }
}

fn action_payload_object(args: &RouterArgs) -> Map<String, Value> {
    let mut object = args.action.extra.clone();
    insert_string(&mut object, "cmd", args.action.cmd.as_ref());
    if let Some(command) = args.action.command.as_ref() {
        object.insert("command".to_string(), command.clone());
    }
    if let Some(commands) = args.action.commands.as_ref() {
        object.insert("commands".to_string(), json!(commands));
    }
    if let Some(paths) = args.action.paths.as_ref() {
        object.insert("paths".to_string(), json!(paths));
    }
    insert_string(&mut object, "chars", args.action.chars.as_ref());
    insert_string(&mut object, "query", args.action.query.as_ref());
    insert_string(&mut object, "target", args.action.target.as_ref());
    insert_string(&mut object, "path", args.action.path.as_ref());
    insert_string(&mut object, "dir_path", args.action.dir_path.as_ref());
    insert_string(&mut object, "detail", args.action.detail.as_ref());
    insert_i64(&mut object, "session_id", args.action.session_id);
    insert_i64(&mut object, "timeout_ms", args.action.timeout_ms);
    insert_bool(&mut object, "wait_until_exit", args.action.wait_until_exit);
    insert_i64(&mut object, "wait_timeout_ms", args.action.wait_timeout_ms);
    insert_i64(&mut object, "yield_time_ms", args.action.yield_time_ms);
    insert_i64(
        &mut object,
        "max_output_tokens",
        args.action.max_output_tokens,
    );
    if let Some(input) = args.action.input.as_ref() {
        object.insert("input".to_string(), input.clone());
    }
    if let Some(targets) = args.action.targets.as_ref() {
        object.insert("targets".to_string(), json!(targets));
    }
    object
}

pub(super) fn mcp_tool_name(
    args: &RouterArgs,
    index: &ToolRouterIndex,
) -> Result<Option<ToolName>, FunctionCallError> {
    let namespace = args.where_.namespace.as_deref().or_else(|| {
        args.targets
            .iter()
            .find_map(|target| target.namespace.as_deref())
    });
    let candidates = [
        args.action.tool.as_ref(),
        args.action.name.as_ref(),
        first_target_value_ref(&args.targets, "tool"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(tool_name) = index.find_exact(candidate, namespace)? {
            return Ok(Some(tool_name));
        }
    }
    Ok(None)
}

pub(super) fn repo_ci_tool_name(
    kind: &str,
    args: &RouterArgs,
    index: &ToolRouterIndex,
) -> Result<Option<ToolName>, FunctionCallError> {
    let candidates = [
        args.action.tool.as_deref(),
        args.action.name.as_deref(),
        kind.strip_prefix("repo_ci_"),
        Some(kind),
    ];
    for candidate in candidates.into_iter().flatten() {
        if let Some(tool_name) = index.find_exact(candidate, Some("repo_ci"))? {
            return Ok(Some(tool_name));
        }
    }
    Ok(None)
}

pub(super) fn agent_tool_name(kind: &str, index: &ToolRouterIndex) -> Option<ToolName> {
    let candidates: &[&str] = match kind {
        "spawn" | "spawn_agent" => &["spawn_agent"],
        "send" | "send_input" => &["send_input", "send_message", "followup_task"],
        "send_message" => &["send_message"],
        "followup" | "followup_task" => &["followup_task", "send_input"],
        "wait" | "poll" | "wait_agent" => &["wait_agent"],
        "close" | "close_agent" => &["close_agent"],
        "resume" | "resume_agent" => &["resume_agent"],
        "list" | "list_agents" => &["list_agents"],
        "agent" | "direct_tool" | "tool" | "function" | "apply_patch" | "write_stdin"
        | "tool_search" | "view_image" | "exec" | "shell" | "command" | "git" | "mcp" | "image"
        | "read" | "grep" | "find" | "none" | "" => &[],
        _ => &[kind],
    };
    candidates
        .iter()
        .map(|candidate| ToolName::plain(*candidate))
        .find(|tool_name| index.has_handler(tool_name))
}

fn action_text_input(args: &RouterArgs) -> Option<String> {
    args.action
        .patch
        .clone()
        .or_else(|| string_input(&args.action.input))
}

fn string_input(value: &Option<Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Null)
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::Array(_))
        | Some(Value::Object(_))
        | None => None,
    }
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

fn target_paths(targets: &[RouterTarget]) -> Vec<String> {
    targets
        .iter()
        .filter_map(|target| {
            target.path.clone().or_else(|| {
                matches!(
                    target.kind.as_deref().map(normalize).as_deref(),
                    Some("path") | Some("dir") | Some("directory")
                )
                .then(|| target.value.clone())
                .flatten()
            })
        })
        .collect()
}

fn first_agent_target(args: &RouterArgs) -> Option<String> {
    args.targets
        .iter()
        .find(|target| target.kind.as_deref().map(normalize).as_deref() == Some("agent"))
        .and_then(|target| {
            target
                .id
                .clone()
                .or_else(|| target.name.clone())
                .or_else(|| target.value.clone())
        })
}

fn first_target_value(targets: &[RouterTarget], kind: &str) -> Option<String> {
    first_target_value_ref(targets, kind).cloned()
}

fn target_values(targets: &[RouterTarget], kind: &str) -> Vec<String> {
    targets
        .iter()
        .filter(|target| target.kind.as_deref().map(normalize).as_deref() == Some(kind))
        .filter_map(|target| {
            target
                .value
                .clone()
                .or_else(|| target.name.clone())
                .or_else(|| target.id.clone())
                .or_else(|| target.uri.clone())
        })
        .collect()
}

fn first_target_value_ref<'a>(targets: &'a [RouterTarget], kind: &str) -> Option<&'a String> {
    targets
        .iter()
        .find(|target| target.kind.as_deref().map(normalize).as_deref() == Some(kind))
        .and_then(|target| {
            target
                .value
                .as_ref()
                .or(target.name.as_ref())
                .or(target.id.as_ref())
                .or(target.uri.as_ref())
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

fn insert_bool(object: &mut Map<String, Value>, key: &str, value: Option<bool>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::Bool(value));
    }
}

pub(super) fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

pub(super) fn is_apply_patch_kind(kind: &str) -> bool {
    matches!(kind, "apply_patch" | "patch" | "edit")
}

pub(super) fn is_write_stdin_kind(kind: &str) -> bool {
    matches!(kind, "write_stdin" | "stdin" | "poll")
}

pub(super) fn is_tool_search_kind(kind: &str) -> bool {
    matches!(kind, "tool_search" | "search_tools" | "discover_tool")
}

pub(super) fn is_agent_kind(where_kind: &str, kind: &str) -> bool {
    where_kind == "agent"
        || matches!(
            kind,
            "spawn"
                | "spawn_agent"
                | "send"
                | "send_input"
                | "send_message"
                | "followup"
                | "followup_task"
                | "wait"
                | "wait_agent"
                | "close"
                | "close_agent"
                | "resume"
                | "resume_agent"
                | "list_agents"
        )
}

pub(super) fn is_mcp_kind(where_kind: &str, kind: &str) -> bool {
    matches!(where_kind, "mcp" | "app") || kind == "mcp"
}

pub(super) fn is_repo_ci_kind(where_kind: &str, kind: &str) -> bool {
    where_kind == "repo_ci"
        || matches!(
            kind,
            "repo_ci"
                | "repo_ci_status"
                | "repo_ci_learn"
                | "repo_ci_run"
                | "repo_ci_result"
                | "repo_ci_instruction"
        )
}

pub(super) fn is_skill_kind(where_kind: &str, kind: &str) -> bool {
    where_kind == "skill" || matches!(kind, "skill" | "search_skill" | "discover_skill")
}

pub(super) fn is_image_view_kind(where_kind: &str, kind: &str) -> bool {
    where_kind == "image" && matches!(kind, "view" | "view_image" | "open")
}

pub(super) fn is_list_dir_kind(where_kind: &str, kind: &str) -> bool {
    matches!(where_kind, "filesystem" | "workspace") && matches!(kind, "list" | "list_dir")
}

pub(super) fn is_shell_kind(where_kind: &str, kind: &str) -> bool {
    matches!(where_kind, "shell" | "git")
        || matches!(
            kind,
            "exec"
                | "exec_wait"
                | "shell"
                | "command"
                | "batch"
                | "git"
                | "read"
                | "read_many"
                | "inspect"
                | "read_context"
                | "grep"
                | "find"
                | "git_snapshot"
                | "snapshot"
                | "status"
                | "diff"
                | "log"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn list_dir_fanout_builds_one_call_per_path_target() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "list dirs",
            "where": {"kind": "workspace"},
            "targets": [
                {"kind": "path", "path": "a"},
                {"kind": "path", "path": "b"}
            ],
            "action": {"kind": "list", "description": "list", "limit": 20},
            "verbosity": "auto"
        }))
        .expect("router args");

        let calls = fanout_for_list_dir("router-call", &args)
            .expect("fanout")
            .expect("calls");

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, ToolName::plain("list_dir"));
        assert_eq!(calls[0].call_id, "router-call:fanout:0");
        assert_eq!(calls[1].call_id, "router-call:fanout:1");
    }

    #[test]
    fn tool_search_fanout_builds_one_call_per_query_target() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "find tools",
            "where": {"kind": "skill"},
            "targets": [
                {"kind": "query", "value": "calendar"},
                {"kind": "query", "value": "email"}
            ],
            "action": {"kind": "tool_search", "description": "search", "limit": 3},
            "verbosity": "auto"
        }))
        .expect("router args");

        let calls = fanout_for_tool_search("router-call", &args).expect("calls");

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, ToolName::plain("tool_search"));
        assert_eq!(calls[0].call_id, "router-call:fanout:0");
        assert_eq!(calls[1].call_id, "router-call:fanout:1");
    }
}
