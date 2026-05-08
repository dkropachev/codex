use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_deterministic::agent_tool_name;
use crate::tools::routing_deterministic::call_for_agent_tool;
use crate::tools::routing_deterministic::call_for_apply_patch;
use crate::tools::routing_deterministic::call_for_exact_tool;
use crate::tools::routing_deterministic::call_for_list_dir;
use crate::tools::routing_deterministic::call_for_tool_search;
use crate::tools::routing_deterministic::call_for_view_image;
use crate::tools::routing_deterministic::call_for_write_stdin;
use crate::tools::routing_deterministic::exact_tool_name;
use crate::tools::routing_deterministic::fanout_for_list_dir;
use crate::tools::routing_deterministic::fanout_for_tool_search;
use crate::tools::routing_deterministic::fanout_for_view_image;
use crate::tools::routing_deterministic::is_agent_kind;
use crate::tools::routing_deterministic::is_apply_patch_kind;
use crate::tools::routing_deterministic::is_image_view_kind;
use crate::tools::routing_deterministic::is_list_dir_kind;
use crate::tools::routing_deterministic::is_mcp_kind;
use crate::tools::routing_deterministic::is_repo_ci_kind;
use crate::tools::routing_deterministic::is_shell_kind;
use crate::tools::routing_deterministic::is_skill_kind;
use crate::tools::routing_deterministic::is_tool_search_kind;
use crate::tools::routing_deterministic::is_write_stdin_kind;
use crate::tools::routing_deterministic::mcp_tool_name;
use crate::tools::routing_deterministic::normalize;
use crate::tools::routing_deterministic::repo_ci_tool_name;
use crate::tools::routing_learned_rules;
use crate::tools::routing_shell::call_for_shell_like;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_state::ToolRouterRequestShape;
use codex_tools::ToolName;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ToolRouterUsage {
    pub(crate) route_kind: String,
    pub(crate) selected_tools: Vec<String>,
    pub(crate) fallback_prompt_tokens: i64,
    pub(crate) fallback_completion_tokens: i64,
    pub(crate) fanout_call_count: i64,
    pub(crate) request_shape_json: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum RouterResolution {
    SingleTool {
        call: Box<ToolCall>,
        usage: ToolRouterUsage,
    },
    FanOut {
        calls: Vec<ToolCall>,
        usage: ToolRouterUsage,
    },
    Noop {
        message: String,
        usage: ToolRouterUsage,
    },
    InlineOutput {
        message: String,
        success: bool,
        usage: ToolRouterUsage,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct RouterArgs {
    pub(super) request: String,
    #[serde(rename = "where")]
    pub(super) where_: RouterWhere,
    #[serde(default)]
    pub(super) targets: Vec<RouterTarget>,
    pub(super) action: RouterAction,
    #[serde(default)]
    pub(super) verbosity: RouterVerbosity,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct RouterWhere {
    pub(super) kind: String,
    pub(super) namespace: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct RouterTarget {
    pub(super) kind: Option<String>,
    pub(super) name: Option<String>,
    pub(super) id: Option<String>,
    pub(super) path: Option<String>,
    pub(super) uri: Option<String>,
    pub(super) namespace: Option<String>,
    pub(super) value: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct RouterAction {
    pub(super) kind: String,
    #[serde(default)]
    pub(super) description: String,
    pub(super) tool: Option<String>,
    pub(super) name: Option<String>,
    pub(super) cmd: Option<String>,
    pub(super) command: Option<Value>,
    pub(super) commands: Option<Vec<String>>,
    pub(super) paths: Option<Vec<String>>,
    pub(super) patch: Option<String>,
    pub(super) input: Option<Value>,
    pub(super) query: Option<String>,
    pub(super) agent_task: Option<String>,
    pub(super) mcp_args: Option<Value>,
    pub(super) target: Option<String>,
    pub(super) targets: Option<Vec<String>>,
    pub(super) session_id: Option<i64>,
    pub(super) chars: Option<String>,
    pub(super) workdir: Option<String>,
    pub(super) timeout_ms: Option<i64>,
    pub(super) wait_until_exit: Option<bool>,
    pub(super) wait_timeout_ms: Option<i64>,
    pub(super) yield_time_ms: Option<i64>,
    pub(super) max_output_tokens: Option<i64>,
    pub(super) sandbox_permissions: Option<String>,
    pub(super) justification: Option<String>,
    pub(super) prefix_rule: Option<Vec<String>>,
    pub(super) detail: Option<String>,
    pub(super) path: Option<String>,
    pub(super) dir_path: Option<String>,
    pub(super) offset: Option<i64>,
    pub(super) limit: Option<i64>,
    pub(super) depth: Option<i64>,
    #[serde(flatten)]
    pub(super) extra: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub(super) enum RouterVerbosity {
    #[default]
    Auto,
    Brief,
    Normal,
    Full,
}

pub(crate) async fn resolve_router_request(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    arguments: String,
) -> Result<RouterResolution, FunctionCallError> {
    let args: RouterArgs = serde_json::from_str(&arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse tool_router arguments: {err}"))
    })?;
    let kind = normalize(&args.action.kind);
    let where_kind = normalize(&args.where_.kind);
    let request_shape_json = sanitized_request_shape_json(&args);
    let _ = (&args.request, &args.action.description, &args.verbosity);

    macro_rules! resolve {
        ($resolution:expr) => {
            return Ok(with_request_shape(
                $resolution,
                request_shape_json.clone(),
            ));
        };
    }

    if is_noop_request(&args, &kind, &where_kind) {
        resolve!(RouterResolution::Noop {
            message: "No internal tool was executed for this routed request.".to_string(),
            usage: usage("none", Vec::new(), 0),
        });
    }

    if is_apply_patch_kind(&kind) {
        let tool_name = ToolName::plain("apply_patch");
        if index.has_handler(&tool_name) {
            let call = call_for_apply_patch(index, call_id, &args)?;
            resolve!(tool_resolution(call));
        }
    }

    if is_write_stdin_kind(&kind) {
        let tool_name = ToolName::plain("write_stdin");
        if index.has_handler(&tool_name) {
            let call = call_for_write_stdin(call_id, &args)?;
            resolve!(tool_resolution(call));
        }
    }

    if is_process_status_kind(&where_kind, &kind) {
        let process_id = args
            .action
            .session_id
            .and_then(|value| i32::try_from(value).ok());
        resolve!(RouterResolution::InlineOutput {
            message: session
                .services
                .unified_exec_manager
                .process_status_summary(process_id)
                .await,
            success: true,
            usage: usage("deterministic", vec!["process.status".to_string()], 0),
        });
    }

    if is_tool_search_kind(&kind) {
        let tool_name = ToolName::plain("tool_search");
        if index.has_handler(&tool_name) {
            if let Some(calls) = fanout_for_tool_search(call_id.as_str(), &args) {
                resolve!(fanout_resolution("deterministic", calls));
            }
            let call = call_for_tool_search(call_id, &args)?;
            resolve!(tool_resolution(call));
        }
    }

    if is_agent_kind(&where_kind, &kind)
        && let Some(tool_name) = agent_tool_name(&kind, index)
    {
        let call = call_for_agent_tool(call_id, tool_name, &args)?;
        resolve!(tool_resolution(call));
    }

    if is_mcp_kind(&where_kind, &kind)
        && let Some(tool_name) = mcp_tool_name(&args, index)?
    {
        let call = call_for_exact_tool(session, index, call_id, tool_name, &args).await?;
        resolve!(tool_resolution(call));
    }

    if is_repo_ci_kind(&where_kind, &kind)
        && let Some(tool_name) = repo_ci_tool_name(&kind, &args, index)?
    {
        let call = call_for_exact_tool(session, index, call_id, tool_name, &args).await?;
        resolve!(tool_resolution(call));
    }

    if is_image_view_kind(&where_kind, &kind) {
        let tool_name = ToolName::plain("view_image");
        if index.has_handler(&tool_name) {
            if let Some(calls) = fanout_for_view_image(call_id.as_str(), &args)? {
                resolve!(fanout_resolution("deterministic", calls));
            }
            let call = call_for_view_image(call_id, &args)?;
            resolve!(tool_resolution(call));
        }
    }

    if is_list_dir_kind(&where_kind, &kind) {
        let tool_name = ToolName::plain("list_dir");
        if index.has_handler(&tool_name) {
            if let Some(calls) = fanout_for_list_dir(call_id.as_str(), &args)? {
                resolve!(fanout_resolution("deterministic", calls));
            }
            let call = call_for_list_dir(call_id, &args)?;
            resolve!(tool_resolution(call));
        }
    }

    if (is_shell_kind(&where_kind, &kind)
        || args.action.cmd.is_some()
        || args.action.command.is_some()
        || args.action.commands.is_some()
        || args.action.paths.is_some())
        && let Some(call) = call_for_shell_like(index, call_id.clone(), &args)?
    {
        resolve!(tool_resolution(call));
    }

    if let Some(tool_name) = exact_tool_name(&args, index)? {
        let call = call_for_exact_tool(session, index, call_id, tool_name, &args).await?;
        resolve!(tool_resolution(call));
    }

    if is_skill_kind(&where_kind, &kind) && index.has_handler(&ToolName::plain("tool_search")) {
        let call = call_for_tool_search(call_id, &args)?;
        resolve!(tool_resolution(call));
    }

    if let Some(resolution) =
        routing_learned_rules::resolve_learned_rule(session, index, call_id.clone(), &args).await?
    {
        resolve!(resolution);
    }

    Err(FunctionCallError::RespondToModel(
        "tool_router could not deterministically route this request. Provide an exact internal tool name in action.tool or a concrete shell cmd."
            .to_string(),
    ))
}

pub(crate) fn sanitized_request_shape_json_from_arguments(arguments: &str) -> Option<String> {
    serde_json::from_str::<RouterArgs>(arguments)
        .ok()
        .and_then(|args| sanitized_request_shape_json(&args))
}

fn sanitized_request_shape_json(args: &RouterArgs) -> Option<String> {
    serde_json::to_string(&ToolRouterRequestShape {
        where_kind: sanitize_known_kind(&args.where_.kind, ROUTER_WHERE_KINDS),
        action_kind: sanitize_known_kind(&args.action.kind, ROUTER_ACTION_KINDS),
        target_kinds: args
            .targets
            .iter()
            .filter_map(|target| target.kind.as_deref())
            .map(|kind| sanitize_known_kind(kind, ROUTER_TARGET_KINDS))
            .filter(|kind| !kind.is_empty())
            .collect(),
        payload_fields: router_action_payload_fields(&args.action),
    })
    .ok()
}

const ROUTER_WHERE_KINDS: &[&str] = &[
    "none",
    "workspace",
    "filesystem",
    "shell",
    "git",
    "repo_ci",
    "process",
    "mcp",
    "app",
    "skill",
    "web",
    "image",
    "agent",
    "memory",
    "config",
];

const ROUTER_TARGET_KINDS: &[&str] = &[
    "tool",
    "path",
    "uri",
    "agent",
    "server",
    "namespace",
    "query",
    "text",
];

const ROUTER_ACTION_KINDS: &[&str] = &[
    "none",
    "exec",
    "exec_command",
    "exec_wait",
    "batch",
    "inspect",
    "read",
    "read_many",
    "list",
    "git_snapshot",
    "repo_ci",
    "repo_ci_status",
    "repo_ci_learn",
    "repo_ci_run",
    "repo_ci_result",
    "repo_ci_instruction",
    "status",
    "diff",
    "log",
    "grep",
    "find",
    "git",
    "update_plan",
    "apply_patch",
    "write_stdin",
    "mcp",
    "spawn_agent",
    "wait_agent",
    "tool_search",
    "view_image",
    "direct_tool",
    "shell",
    "process_status",
    "session_status",
];

fn is_noop_request(args: &RouterArgs, kind: &str, where_kind: &str) -> bool {
    (kind == "none" || (kind == "status" && where_kind == "none"))
        && !has_exact_tool_request(args)
        && !has_actionable_payload(args)
}

fn has_exact_tool_request(args: &RouterArgs) -> bool {
    args.action.tool.is_some()
        || args.action.name.is_some()
        || args.targets.iter().any(|target| {
            target.kind.as_deref().map(normalize).as_deref() == Some("tool")
                && (target.name.is_some() || target.id.is_some() || target.value.is_some())
        })
}

fn has_actionable_payload(args: &RouterArgs) -> bool {
    let action = &args.action;
    action.cmd.is_some()
        || action.command.is_some()
        || action.commands.is_some()
        || action.paths.is_some()
        || action.patch.is_some()
        || action.input.is_some()
        || action.query.is_some()
        || action.agent_task.is_some()
        || action.mcp_args.is_some()
        || action.target.is_some()
        || action.targets.is_some()
        || action.session_id.is_some()
        || action.chars.is_some()
        || action.path.is_some()
        || action.dir_path.is_some()
}

fn sanitize_known_kind(value: &str, known_values: &[&str]) -> String {
    let sanitized = sanitize_shape_value(value);
    if known_values.contains(&sanitized.as_str()) {
        sanitized
    } else {
        "other".to_string()
    }
}

fn sanitize_shape_value(value: &str) -> String {
    value
        .trim()
        .chars()
        .take(64)
        .filter_map(|ch| match ch {
            'a'..='z' | '0'..='9' | '_' | '-' | '.' => Some(ch),
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn router_action_payload_fields(action: &RouterAction) -> Vec<String> {
    let mut fields = Vec::new();
    macro_rules! push_if_some {
        ($field:ident) => {
            if action.$field.is_some() {
                fields.push(stringify!($field).to_string());
            }
        };
    }
    push_if_some!(tool);
    push_if_some!(name);
    push_if_some!(cmd);
    push_if_some!(command);
    push_if_some!(commands);
    push_if_some!(paths);
    push_if_some!(patch);
    push_if_some!(input);
    push_if_some!(query);
    push_if_some!(agent_task);
    push_if_some!(mcp_args);
    push_if_some!(target);
    push_if_some!(targets);
    push_if_some!(session_id);
    push_if_some!(chars);
    push_if_some!(workdir);
    push_if_some!(timeout_ms);
    push_if_some!(wait_until_exit);
    push_if_some!(wait_timeout_ms);
    push_if_some!(yield_time_ms);
    push_if_some!(max_output_tokens);
    push_if_some!(sandbox_permissions);
    push_if_some!(justification);
    push_if_some!(prefix_rule);
    push_if_some!(detail);
    push_if_some!(path);
    push_if_some!(dir_path);
    push_if_some!(offset);
    push_if_some!(limit);
    push_if_some!(depth);
    fields
}

fn is_process_status_kind(where_kind: &str, kind: &str) -> bool {
    where_kind == "process"
        || matches!(kind, "process_status" | "session_status")
        || (where_kind == "shell" && kind == "status")
}

pub(crate) fn response_to_content_items(
    response: ResponseInputItem,
) -> Vec<FunctionCallOutputContentItem> {
    match response {
        ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } => {
            body_to_content_items(output.body)
        }
        ResponseInputItem::McpToolCallOutput { output, .. } => {
            body_to_content_items(output.as_function_call_output_payload().body)
        }
        ResponseInputItem::ToolSearchOutput { tools, .. } => {
            let text = serde_json::to_string(&tools)
                .unwrap_or_else(|err| format!("failed to serialize tool_search output: {err}"));
            vec![FunctionCallOutputContentItem::InputText { text }]
        }
        ResponseInputItem::Message { content, .. } => content
            .into_iter()
            .map(|item| match item {
                codex_protocol::models::ContentItem::InputText { text }
                | codex_protocol::models::ContentItem::OutputText { text } => {
                    FunctionCallOutputContentItem::InputText { text }
                }
                codex_protocol::models::ContentItem::InputImage { image_url, detail } => {
                    FunctionCallOutputContentItem::InputImage { image_url, detail }
                }
            })
            .collect(),
    }
}

fn body_to_content_items(body: FunctionCallOutputBody) -> Vec<FunctionCallOutputContentItem> {
    match body {
        FunctionCallOutputBody::Text(text) => {
            vec![FunctionCallOutputContentItem::InputText { text }]
        }
        FunctionCallOutputBody::ContentItems(items) => items,
    }
}

fn usage(route_kind: &str, selected_tools: Vec<String>, fanout_call_count: i64) -> ToolRouterUsage {
    ToolRouterUsage {
        route_kind: route_kind.to_string(),
        selected_tools,
        fallback_prompt_tokens: 0,
        fallback_completion_tokens: 0,
        fanout_call_count,
        request_shape_json: None,
    }
}

fn with_request_shape(
    mut resolution: RouterResolution,
    request_shape_json: Option<String>,
) -> RouterResolution {
    match &mut resolution {
        RouterResolution::SingleTool { usage, .. }
        | RouterResolution::FanOut { usage, .. }
        | RouterResolution::Noop { usage, .. }
        | RouterResolution::InlineOutput { usage, .. } => {
            usage.request_shape_json = request_shape_json;
        }
    }
    resolution
}

fn tool_resolution(call: ToolCall) -> RouterResolution {
    let selected_tool = call.tool_name.display();
    RouterResolution::SingleTool {
        call: Box::new(call),
        usage: usage("deterministic", vec![selected_tool], 1),
    }
}

pub(super) fn route_resolution(
    route_kind: &str,
    call: ToolCall,
    fallback_prompt_tokens: i64,
    fallback_completion_tokens: i64,
) -> RouterResolution {
    let selected_tool = call.tool_name.display();
    RouterResolution::SingleTool {
        call: Box::new(call),
        usage: ToolRouterUsage {
            route_kind: route_kind.to_string(),
            selected_tools: vec![selected_tool],
            fallback_prompt_tokens,
            fallback_completion_tokens,
            fanout_call_count: 1,
            request_shape_json: None,
        },
    }
}

pub(super) fn fanout_resolution(route_kind: &str, calls: Vec<ToolCall>) -> RouterResolution {
    let selected_tools = calls.iter().map(|call| call.tool_name.display()).collect();
    RouterResolution::FanOut {
        usage: usage(
            route_kind,
            selected_tools,
            i64::try_from(calls.len()).unwrap_or(i64::MAX),
        ),
        calls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::context::FunctionToolOutput;
    use crate::tools::context::ToolInvocation;
    use crate::tools::context::ToolPayload;
    use crate::tools::registry::ToolHandler;
    use crate::tools::registry::ToolKind;
    use crate::tools::registry::ToolRegistry;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::collections::HashSet;
    use std::sync::Arc;

    struct TestHandler;

    impl ToolHandler for TestHandler {
        type Output = FunctionToolOutput;

        fn kind(&self) -> ToolKind {
            ToolKind::Function
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
        ) -> Result<FunctionToolOutput, FunctionCallError> {
            Ok(FunctionToolOutput::from_text("ok".to_string(), Some(true)))
        }
    }

    fn index_with_tool(name: &str) -> ToolRouterIndex {
        let registry =
            ToolRegistry::with_handler_for_test(ToolName::plain(name), Arc::new(TestHandler));
        ToolRouterIndex::build(&[], &registry, &HashSet::new())
    }

    fn function_arguments(call: &ToolCall) -> Value {
        let ToolPayload::Function { arguments } = &call.payload else {
            panic!("expected function payload")
        };
        serde_json::from_str(arguments).expect("function arguments")
    }

    #[test]
    fn response_to_content_items_preserves_function_text() {
        let items = response_to_content_items(ResponseInputItem::FunctionCallOutput {
            call_id: "call".to_string(),
            output: codex_protocol::models::FunctionCallOutputPayload::from_text("ok".to_string()),
        });

        assert_eq!(
            items,
            vec![FunctionCallOutputContentItem::InputText {
                text: "ok".to_string()
            }]
        );
    }

    #[test]
    fn router_args_default_optional_description_and_verbosity() {
        let args: RouterArgs = serde_json::from_value(json!({
            "request": "status",
            "where": {"kind": "process"},
            "action": {"kind": "status"}
        }))
        .expect("router args");

        assert_eq!(args.action.description, "");
        assert!(matches!(args.verbosity, RouterVerbosity::Auto));
    }

    #[test]
    fn sanitized_request_shape_omits_request_text_and_payload_values() {
        let arguments = json!({
            "request": "read the secret token from /tmp/private.txt",
            "where": {"kind": "Shell"},
            "targets": [{"kind": "path", "path": "/tmp/private.txt"}],
            "action": {"kind": "exec", "cmd": "cat /tmp/private.txt", "workdir": "/tmp"}
        })
        .to_string();

        let shape_json =
            sanitized_request_shape_json_from_arguments(&arguments).expect("shape json");
        let shape: ToolRouterRequestShape = serde_json::from_str(&shape_json).expect("shape");

        assert_eq!(
            shape,
            ToolRouterRequestShape {
                where_kind: "shell".to_string(),
                action_kind: "exec".to_string(),
                target_kinds: vec!["path".to_string()],
                payload_fields: vec!["cmd".to_string(), "workdir".to_string()],
            }
        );
        assert!(!shape_json.contains("secret"));
        assert!(!shape_json.contains("private"));
        assert!(!shape_json.contains("cat"));
    }

    #[tokio::test]
    async fn exact_spawn_agent_uses_agent_adapter_for_message() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "delegate",
            "where": {"kind": "agent"},
            "targets": [],
            "action": {
                "kind": "spawn_agent",
                "tool": "spawn_agent",
                "agent_task": "inspect routing"
            }
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("spawn_agent"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, .. } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("spawn_agent"));
        assert_eq!(
            function_arguments(&call),
            json!({"message": "inspect routing"})
        );
    }

    #[tokio::test]
    async fn exact_list_dir_uses_adapter_for_path_target() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "list workspace",
            "where": {"kind": "workspace"},
            "targets": [{"kind": "path", "path": "codex-rs/core"}],
            "action": {"kind": "list", "tool": "list_dir", "limit": 20}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("list_dir"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, .. } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("list_dir"));
        assert_eq!(
            function_arguments(&call),
            json!({"dir_path": "codex-rs/core", "limit": 20})
        );
    }

    #[tokio::test]
    async fn exact_view_image_uses_adapter_for_path_target() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "view image",
            "where": {"kind": "image"},
            "targets": [{"kind": "path", "path": "screenshot.png"}],
            "action": {"kind": "view_image", "tool": "view_image", "detail": "original"}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("view_image"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, .. } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("view_image"));
        assert_eq!(
            function_arguments(&call),
            json!({"path": "screenshot.png", "detail": "original"})
        );
    }

    #[tokio::test]
    async fn exact_tool_search_uses_adapter_for_fanout() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "find tools",
            "where": {"kind": "skill"},
            "targets": [
                {"kind": "query", "value": "calendar"},
                {"kind": "query", "value": "email"}
            ],
            "action": {"kind": "tool_search", "tool": "tool_search", "limit": 3}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("tool_search"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::FanOut { calls, .. } = resolution else {
            panic!("expected fanout resolution")
        };

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, ToolName::plain("tool_search"));
        assert_eq!(calls[0].call_id, "router-call:fanout:0");
        assert_eq!(calls[1].call_id, "router-call:fanout:1");
    }

    #[tokio::test]
    async fn none_where_update_plan_input_routes_exact_tool() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "track progress",
            "where": {"kind": "none"},
            "targets": [],
            "action": {
                "kind": "update_plan",
                "input": {"plan": [{"step": "inspect", "status": "in_progress"}]}
            }
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("update_plan"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, usage } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("update_plan"));
        assert_eq!(
            function_arguments(&call),
            json!({"plan": [{"step": "inspect", "status": "in_progress"}]})
        );
        assert!(usage.request_shape_json.is_some());
    }

    #[tokio::test]
    async fn none_where_exec_command_cmd_routes_shell() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "print date",
            "where": {"kind": "none"},
            "targets": [],
            "action": {"kind": "exec_command", "cmd": "date"}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("exec_command"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, .. } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("exec_command"));
        assert_eq!(function_arguments(&call), json!({"cmd": "date"}));
    }

    #[tokio::test]
    async fn filesystem_exec_command_cmd_routes_shell() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "print cwd",
            "where": {"kind": "filesystem"},
            "targets": [],
            "action": {"kind": "exec_command", "cmd": "pwd"}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("exec_command"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, .. } = resolution else {
            panic!("expected single tool resolution")
        };

        assert_eq!(call.tool_name, ToolName::plain("exec_command"));
        assert_eq!(function_arguments(&call), json!({"cmd": "pwd"}));
    }

    #[tokio::test]
    async fn action_kind_none_without_payload_returns_noop() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "nothing to do",
            "where": {"kind": "none"},
            "targets": [],
            "action": {"kind": "none"}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("exec_command"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::Noop { usage, .. } = resolution else {
            panic!("expected noop resolution")
        };

        assert_eq!(usage.route_kind, "none");
        assert!(usage.request_shape_json.is_some());
    }

    #[tokio::test]
    async fn multi_path_batch_routes_to_one_shell_call() {
        let (session, _) = crate::session::tests::make_session_and_context().await;
        let args = json!({
            "request": "read files",
            "where": {"kind": "filesystem"},
            "targets": [
                {"kind": "path", "path": "a.txt"},
                {"kind": "path", "path": "b.txt"}
            ],
            "action": {"kind": "batch"}
        })
        .to_string();

        let resolution = resolve_router_request(
            &session,
            &index_with_tool("exec_command"),
            "router-call".to_string(),
            args,
        )
        .await
        .expect("resolution");
        let RouterResolution::SingleTool { call, usage } = resolution else {
            panic!("expected one shell call")
        };

        assert_eq!(call.tool_name, ToolName::plain("exec_command"));
        let arguments = function_arguments(&call);
        let command = arguments["cmd"].as_str().expect("cmd");
        assert!(command.contains("## path a.txt"));
        assert!(command.contains("## path b.txt"));
        assert_eq!(usage.fanout_call_count, 1);
    }
}
