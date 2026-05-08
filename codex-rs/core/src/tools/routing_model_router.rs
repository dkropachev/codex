use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::function_tool::FunctionCallError;
use crate::model_router::ModelRouterSource;
use crate::model_router::apply_model_router;
use crate::model_router::auth_manager_for_config;
use crate::model_router::available_router_models;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_deterministic;
use crate::tools::routing_shell;
use crate::tools::routing_tool;
use crate::tools::routing_tool::RouterArgs;
use crate::tools::routing_tool::RouterResolution;
use codex_features::Feature;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const MAX_SCRIPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ModelRouterToolCall {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LearnedRoute {
    Route { tool: String, arguments: Value },
    Fanout { calls: Vec<ModelRouterToolCall> },
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelRouterDecision {
    Route {
        tool: String,
        #[serde(default)]
        arguments: Value,
        #[serde(default)]
        persist_rule: bool,
    },
    Rule {
        tool: String,
        #[serde(default)]
        arguments: Value,
    },
    Fanout {
        calls: Vec<ModelRouterToolCall>,
        #[serde(default)]
        persist_rule: bool,
    },
    Script {
        script: String,
    },
    NoRoute {
        reason: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
struct ModelRouterToolCallWire {
    tool: String,
    #[serde(default)]
    arguments_json: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelRouterDecisionWire {
    Route {
        tool: String,
        #[serde(default)]
        arguments_json: Option<String>,
        #[serde(default)]
        persist_rule: Option<bool>,
    },
    Rule {
        tool: String,
        #[serde(default)]
        arguments_json: Option<String>,
    },
    Fanout {
        calls: Vec<ModelRouterToolCallWire>,
        #[serde(default)]
        persist_rule: Option<bool>,
    },
    Script {
        script: String,
    },
    NoRoute {
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, Default)]
struct ModelRouterRouteUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
}

pub(super) async fn resolve_learned_rule(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
) -> Result<Option<RouterResolution>, FunctionCallError> {
    let Some(state_db) = session.services.state_db.as_deref() else {
        return Ok(None);
    };
    let match_key = learned_rule_match_key(args);
    let rule = state_db
        .lookup_tool_router_rule(&match_key)
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to read tool_router learned rule: {err}"
            ))
        })?;
    let Some(rule) = rule else {
        return Ok(None);
    };

    let route: LearnedRoute = serde_json::from_str(&rule.route_json).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid learned tool_router rule: {err}"))
    })?;
    let resolution = resolve_learned_route(session, index, call_id, args, route).await?;
    if let Err(err) = state_db.record_tool_router_rule_hit(&match_key).await {
        tracing::warn!("failed to record tool_router learned rule hit: {err}");
    }
    Ok(Some(resolution))
}

pub(super) async fn resolve_with_model_router(
    session: &Session,
    turn: &TurnContext,
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
) -> Result<Option<RouterResolution>, FunctionCallError> {
    if !turn
        .config
        .model_router
        .as_ref()
        .is_some_and(|model_router| model_router.enabled)
    {
        return Ok(None);
    }

    let prompt_text = model_router_user_prompt(args, index);
    let available_models = available_router_models(&session.services.models_manager);
    let mut routed_config = turn.config.as_ref().clone();
    apply_model_router(
        &mut routed_config,
        ModelRouterSource::Module("tool_router.resolve"),
        prompt_text.len(),
        &available_models,
    )
    .map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to apply tool_router model router config: {err}"
        ))
    })?;

    let (decision, usage) =
        model_router_decision(session, turn, prompt_text, &routed_config).await?;
    let persist_route = match &decision {
        ModelRouterDecision::Route {
            tool,
            arguments,
            persist_rule,
        } if *persist_rule => Some(LearnedRoute::Route {
            tool: tool.clone(),
            arguments: arguments.clone(),
        }),
        ModelRouterDecision::Rule { tool, arguments } => Some(LearnedRoute::Route {
            tool: tool.clone(),
            arguments: arguments.clone(),
        }),
        ModelRouterDecision::Fanout {
            calls,
            persist_rule,
        } if *persist_rule => Some(LearnedRoute::Fanout {
            calls: calls.clone(),
        }),
        ModelRouterDecision::Route { .. }
        | ModelRouterDecision::Fanout { .. }
        | ModelRouterDecision::Script { .. }
        | ModelRouterDecision::NoRoute { .. } => None,
    };

    let resolution = match decision {
        ModelRouterDecision::Route {
            tool, arguments, ..
        }
        | ModelRouterDecision::Rule { tool, arguments } => {
            let call = call_for_model_router_tool(
                session,
                index,
                call_id,
                args,
                ModelRouterToolCall { tool, arguments },
            )
            .await?;
            routing_tool::route_resolution(
                "model_router",
                call,
                usage.prompt_tokens,
                usage.completion_tokens,
            )
        }
        ModelRouterDecision::Fanout { calls, .. } => {
            let calls =
                calls_for_model_router_fanout(session, index, call_id.as_str(), args, calls)
                    .await?;
            routing_tool::model_router_fanout_resolution(
                calls,
                usage.prompt_tokens,
                usage.completion_tokens,
            )
        }
        ModelRouterDecision::Script { script } => {
            let call = call_for_model_router_script(index, call_id, args, script)?;
            routing_tool::model_router_script_resolution(
                call,
                usage.prompt_tokens,
                usage.completion_tokens,
            )
        }
        ModelRouterDecision::NoRoute { reason } => {
            return Err(FunctionCallError::RespondToModel(format!(
                "tool_router model-router fallback could not route this request: {reason}"
            )));
        }
    };

    if let Some(route) = persist_route {
        persist_learned_route(session, args, route).await;
    }
    Ok(Some(resolution))
}

async fn model_router_decision(
    session: &Session,
    turn: &TurnContext,
    prompt_text: String,
    config: &Config,
) -> Result<(ModelRouterDecision, ModelRouterRouteUsage), FunctionCallError> {
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| turn.model_info.slug.clone());
    let model_info = session
        .services
        .models_manager
        .get_model_info(model.as_str(), &config.to_models_manager_config())
        .await;
    let auth_manager = Some(auth_manager_for_config(
        config,
        &session.services.auth_manager,
    ));
    let client = ModelClient::new(
        auth_manager,
        session.conversation_id,
        "tool-router".to_string(),
        config.model_provider.clone(),
        turn.session_source.clone(),
        config.model_verbosity,
        config.features.enabled(Feature::EnableRequestCompression),
        config.features.enabled(Feature::RuntimeMetrics),
        crate::session::session::Session::build_model_client_beta_features_header(config),
    );
    let mut client_session = client.new_session();
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: prompt_text }],
            phase: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: model_router_instructions(),
        },
        personality: None,
        output_schema: Some(model_router_output_schema()),
        output_schema_strict: true,
    };

    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            &turn.session_telemetry,
            config.model_reasoning_effort,
            config
                .model_reasoning_summary
                .unwrap_or(turn.reasoning_summary),
            config.service_tier,
            None,
            &codex_rollout_trace::InferenceTraceContext::disabled(),
        )
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "tool_router model-router fallback failed: {err}"
            ))
        })?;

    let mut output_text = String::new();
    let mut delta_text = String::new();
    let mut usage = ModelRouterRouteUsage::default();
    while let Some(event) = stream.next().await {
        match event.map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "tool_router model-router fallback stream failed: {err}"
            ))
        })? {
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                output_text = message_text(&content);
            }
            ResponseEvent::OutputTextDelta(delta) => delta_text.push_str(&delta),
            ResponseEvent::Completed { token_usage, .. } => {
                if let Some(token_usage) = token_usage {
                    usage = usage_from_tokens(&token_usage);
                }
                break;
            }
            ResponseEvent::Created
            | ResponseEvent::OutputItemAdded(_)
            | ResponseEvent::OutputItemDone(_)
            | ResponseEvent::ServerModel(_)
            | ResponseEvent::ModelVerifications(_)
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::ToolCallInputDelta { .. }
            | ResponseEvent::ReasoningSummaryDelta { .. }
            | ResponseEvent::ReasoningContentDelta { .. }
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::RateLimits(_)
            | ResponseEvent::ModelsEtag(_) => {}
        }
    }

    if output_text.trim().is_empty() {
        output_text = delta_text;
    }
    let decision = parse_model_router_decision(&output_text)?;
    Ok((decision, usage))
}

async fn resolve_learned_route(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
    route: LearnedRoute,
) -> Result<RouterResolution, FunctionCallError> {
    match route {
        LearnedRoute::Route { tool, arguments } => {
            let call = call_for_model_router_tool(
                session,
                index,
                call_id,
                args,
                ModelRouterToolCall { tool, arguments },
            )
            .await?;
            Ok(routing_tool::route_resolution("learned_rule", call, 0, 0))
        }
        LearnedRoute::Fanout { calls } => {
            let calls =
                calls_for_model_router_fanout(session, index, call_id.as_str(), args, calls)
                    .await?;
            Ok(routing_tool::fanout_resolution("learned_rule", calls))
        }
    }
}

async fn call_for_model_router_tool(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    base_args: &RouterArgs,
    call: ModelRouterToolCall,
) -> Result<ToolCall, FunctionCallError> {
    let tool_name = index.find_exact(&call.tool, None)?.ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "tool_router model-router fallback selected unknown tool `{}`",
            call.tool
        ))
    })?;
    if tool_name.name == "tool_router" {
        return Err(FunctionCallError::RespondToModel(
            "tool_router model-router fallback may not route to tool_router".to_string(),
        ));
    }
    let synthetic_args = synthetic_router_args(base_args, &call)?;
    routing_deterministic::call_for_exact_tool(session, index, call_id, tool_name, &synthetic_args)
        .await
}

async fn calls_for_model_router_fanout(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: &str,
    base_args: &RouterArgs,
    calls: Vec<ModelRouterToolCall>,
) -> Result<Vec<ToolCall>, FunctionCallError> {
    if calls.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "tool_router model-router fallback fanout route requires at least one call".to_string(),
        ));
    }
    let mut routed = Vec::with_capacity(calls.len());
    for (index_value, call) in calls.into_iter().enumerate() {
        let routed_call = call_for_model_router_tool(
            session,
            index,
            routing_deterministic::fanout_call_id(call_id, index_value),
            base_args,
            call,
        )
        .await?;
        if !index.fanout_safe(&routed_call.tool_name) {
            return Err(FunctionCallError::RespondToModel(format!(
                "tool_router fanout route rejected mutating or non-parallel-safe tool `{}`",
                routed_call.tool_name.display()
            )));
        }
        routed.push(routed_call);
    }
    Ok(routed)
}

fn call_for_model_router_script(
    index: &ToolRouterIndex,
    call_id: String,
    base_args: &RouterArgs,
    script: String,
) -> Result<ToolCall, FunctionCallError> {
    if script.trim().is_empty() || script.len() > MAX_SCRIPT_BYTES || script.contains('\0') {
        return Err(FunctionCallError::RespondToModel(
            "tool_router model-router fallback produced an invalid shell script".to_string(),
        ));
    }
    let synthetic_args = synthetic_script_args(base_args, script)?;
    routing_shell::call_for_shell_like(index, call_id, &synthetic_args)?.ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "tool_router model-router fallback produced a script but no shell tool is available"
                .to_string(),
        )
    })
}

async fn persist_learned_route(session: &Session, args: &RouterArgs, route: LearnedRoute) {
    let Some(state_db) = session.services.state_db.as_deref() else {
        return;
    };
    let route_json = match serde_json::to_string(&route) {
        Ok(route_json) => route_json,
        Err(err) => {
            tracing::warn!("failed to serialize tool_router learned rule: {err}");
            return;
        }
    };
    if let Err(err) = state_db
        .upsert_tool_router_rule(&learned_rule_match_key(args), &route_json, "model_router")
        .await
    {
        tracing::warn!("failed to persist tool_router learned rule: {err}");
    }
}

fn synthetic_router_args(
    base_args: &RouterArgs,
    call: &ModelRouterToolCall,
) -> Result<RouterArgs, FunctionCallError> {
    let mut action = Map::new();
    action.insert("kind".to_string(), Value::String("direct_tool".to_string()));
    action.insert(
        "description".to_string(),
        Value::String("model-router-selected route".to_string()),
    );
    action.insert("tool".to_string(), Value::String(call.tool.clone()));
    action.insert("input".to_string(), call.arguments.clone());
    action.insert("mcp_args".to_string(), call.arguments.clone());
    if let Value::Object(arguments) = &call.arguments {
        for (key, value) in arguments {
            action.insert(key.clone(), value.clone());
        }
    }
    router_args_from_value(json!({
        "request": base_args.request.clone(),
        "where": {
            "kind": base_args.where_.kind.clone(),
            "namespace": base_args.where_.namespace.clone(),
        },
        "targets": base_args.targets.clone(),
        "action": action,
        "verbosity": "auto",
    }))
}

fn synthetic_script_args(
    base_args: &RouterArgs,
    script: String,
) -> Result<RouterArgs, FunctionCallError> {
    router_args_from_value(json!({
        "request": base_args.request.clone(),
        "where": {"kind": "shell"},
        "targets": base_args.targets.clone(),
        "action": {
            "kind": "shell",
            "description": "model-router-generated shell fallback",
            "cmd": script,
        },
        "verbosity": "auto",
    }))
}

fn router_args_from_value(value: Value) -> Result<RouterArgs, FunctionCallError> {
    serde_json::from_value(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid model-router route: {err}"))
    })
}

fn learned_rule_match_key(args: &RouterArgs) -> String {
    let mut parts = vec![
        "v1".to_string(),
        format!("where={}", key_part(&args.where_.kind)),
        format!("action={}", key_part(&args.action.kind)),
    ];
    if let Some(tool) = args.action.tool.as_deref().or(args.action.name.as_deref()) {
        parts.push(format!("tool={}", key_part(tool)));
    }
    if let Some(target) = args.targets.first() {
        if let Some(kind) = target.kind.as_deref() {
            parts.push(format!("target_kind={}", key_part(kind)));
        }
        if let Some(value) = target
            .path
            .as_deref()
            .or(target.uri.as_deref())
            .or(target.name.as_deref())
            .or(target.id.as_deref())
            .or(target.value.as_deref())
        {
            parts.push(format!("target={}", key_part(value)));
        }
    }
    parts.join("|")
}

fn key_part(value: &str) -> String {
    value
        .trim()
        .chars()
        .take(160)
        .map(|ch| match ch {
            '|' | '\n' | '\r' | '\t' => '_',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn model_router_instructions() -> String {
    "You are Codex's internal tool router fallback. Return only JSON matching the provided schema. Prefer an existing tool route. Return script only when no existing tool can satisfy the request. Never choose tool_router. Put tool arguments in arguments_json as a JSON string, such as \"{\\\"cmd\\\":\\\"git status --short\\\"}\". Use \"{}\" when the selected tool does not need arguments. Set unused nullable fields to null.".to_string()
}

fn model_router_user_prompt(args: &RouterArgs, index: &ToolRouterIndex) -> String {
    let request_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
    let catalog = index.prompt_catalog().join("\n");
    format!(
        "Route this tool_router request.\n\nAvailable tools:\n{catalog}\n\nRequest JSON:\n{request_json}"
    )
}

fn model_router_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["type", "tool", "arguments_json", "persist_rule", "calls", "script", "reason"],
        "properties": {
            "type": {
                "type": "string",
                "enum": ["route", "rule", "fanout", "script", "no_route"]
            },
            "tool": {"type": ["string", "null"]},
            "arguments_json": {"type": ["string", "null"]},
            "persist_rule": {"type": ["boolean", "null"]},
            "calls": {
                "type": ["array", "null"],
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["tool", "arguments_json"],
                    "properties": {
                        "tool": {"type": "string"},
                        "arguments_json": {"type": "string"}
                    }
                }
            },
            "script": {"type": ["string", "null"]},
            "reason": {"type": ["string", "null"]}
        }
    })
}

fn parse_model_router_decision(
    output_text: &str,
) -> Result<ModelRouterDecision, FunctionCallError> {
    let decision: ModelRouterDecisionWire =
        serde_json::from_str(output_text.trim()).map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "tool_router model-router fallback returned invalid JSON: {err}"
            ))
        })?;
    decision.try_into()
}

impl TryFrom<ModelRouterDecisionWire> for ModelRouterDecision {
    type Error = FunctionCallError;

    fn try_from(decision: ModelRouterDecisionWire) -> Result<Self, Self::Error> {
        match decision {
            ModelRouterDecisionWire::Route {
                tool,
                arguments_json,
                persist_rule,
            } => Ok(Self::Route {
                tool,
                arguments: parse_arguments_json(arguments_json)?,
                persist_rule: persist_rule.unwrap_or(false),
            }),
            ModelRouterDecisionWire::Rule {
                tool,
                arguments_json,
            } => Ok(Self::Rule {
                tool,
                arguments: parse_arguments_json(arguments_json)?,
            }),
            ModelRouterDecisionWire::Fanout {
                calls,
                persist_rule,
            } => Ok(Self::Fanout {
                calls: calls
                    .into_iter()
                    .map(ModelRouterToolCall::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
                persist_rule: persist_rule.unwrap_or(false),
            }),
            ModelRouterDecisionWire::Script { script } => Ok(Self::Script { script }),
            ModelRouterDecisionWire::NoRoute { reason } => Ok(Self::NoRoute { reason }),
        }
    }
}

impl TryFrom<ModelRouterToolCallWire> for ModelRouterToolCall {
    type Error = FunctionCallError;

    fn try_from(call: ModelRouterToolCallWire) -> Result<Self, Self::Error> {
        Ok(Self {
            tool: call.tool,
            arguments: parse_arguments_json(call.arguments_json)?,
        })
    }
}

fn parse_arguments_json(arguments_json: Option<String>) -> Result<Value, FunctionCallError> {
    let Some(arguments_json) = arguments_json else {
        return Ok(Value::Object(Map::new()));
    };
    let trimmed = arguments_json.trim();
    if trimmed.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(trimmed).map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "tool_router model-router fallback returned invalid arguments_json: {err}"
        ))
    })
}

fn message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn usage_from_tokens(token_usage: &TokenUsage) -> ModelRouterRouteUsage {
    ModelRouterRouteUsage {
        prompt_tokens: token_usage.input_tokens,
        completion_tokens: token_usage
            .output_tokens
            .saturating_add(token_usage.reasoning_output_tokens),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::tools::context::FunctionToolOutput;
    use crate::tools::context::ToolInvocation;
    use crate::tools::context::ToolPayload;
    use crate::tools::registry::ToolHandler;
    use crate::tools::registry::ToolKind;
    use crate::tools::registry::ToolRegistry;
    use codex_tools::ConfiguredToolSpec;
    use codex_tools::JsonSchema;
    use codex_tools::ResponsesApiTool;
    use codex_tools::ToolName;
    use codex_tools::ToolSpec;
    use pretty_assertions::assert_eq;

    use super::*;

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

    #[test]
    fn learned_rule_match_key_is_bounded_and_stable() {
        let args = router_args_from_value(json!({
            "request": "list it",
            "where": {"kind": "workspace"},
            "targets": [{"kind": "path", "path": "src|main.rs"}],
            "action": {"kind": "list", "description": "list"},
            "verbosity": "auto"
        }))
        .expect("args");

        assert_eq!(
            learned_rule_match_key(&args),
            "v1|where=workspace|action=list|target_kind=path|target=src_main.rs"
        );
    }

    #[test]
    fn rejects_invalid_model_router_json() {
        assert!(parse_model_router_decision("not json").is_err());
    }

    #[test]
    fn parses_model_router_arguments_json() {
        let decision = parse_model_router_decision(
            r#"{
                "type": "route",
                "tool": "exec_command",
                "arguments_json": "{\"cmd\":\"git status --short\"}",
                "persist_rule": true,
                "calls": null,
                "script": null,
                "reason": null
            }"#,
        )
        .expect("decision");

        assert_eq!(
            decision,
            ModelRouterDecision::Route {
                tool: "exec_command".to_string(),
                arguments: json!({"cmd": "git status --short"}),
                persist_rule: true,
            }
        );
    }

    #[test]
    fn model_router_output_schema_is_strict() {
        fn assert_strict_objects(value: &Value) {
            if value.get("type") == Some(&json!("object")) {
                assert_eq!(value.get("additionalProperties"), Some(&json!(false)));
            }
            match value {
                Value::Array(items) => {
                    for item in items {
                        assert_strict_objects(item);
                    }
                }
                Value::Object(map) => {
                    for value in map.values() {
                        assert_strict_objects(value);
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
            }
        }

        assert_strict_objects(&model_router_output_schema());
    }

    #[test]
    fn model_router_script_routes_through_existing_shell_handler() {
        let index = index_with_handler("exec_command");
        let args = router_args_from_value(json!({
            "request": "print",
            "where": {"kind": "workspace"},
            "targets": [],
            "action": {"kind": "unknown", "description": "print"},
            "verbosity": "auto"
        }))
        .expect("args");

        let call = call_for_model_router_script(
            &index,
            "call-model-router".to_string(),
            &args,
            "printf hi".to_string(),
        )
        .expect("script route");

        assert_eq!(call.tool_name, ToolName::plain("exec_command"));
        match call.payload {
            ToolPayload::Function { arguments } => {
                let value: Value = serde_json::from_str(&arguments).expect("arguments json");
                assert_eq!(value, json!({"cmd": "printf hi"}));
            }
            other => panic!("expected function payload, got {other:?}"),
        }
    }

    #[test]
    fn model_router_script_rejects_invalid_script_before_shell_lookup() {
        let index =
            ToolRouterIndex::build(&[], &ToolRegistry::empty_for_test(), &Default::default());
        let args = router_args_from_value(json!({
            "request": "print",
            "where": {"kind": "workspace"},
            "targets": [],
            "action": {"kind": "unknown", "description": "print"},
            "verbosity": "auto"
        }))
        .expect("args");

        assert!(
            call_for_model_router_script(&index, "call-empty".to_string(), &args, String::new())
                .is_err()
        );
        assert!(
            call_for_model_router_script(&index, "call-nul".to_string(), &args, "\0".to_string())
                .is_err()
        );
    }

    fn index_with_handler(name: &str) -> ToolRouterIndex {
        let tool_name = ToolName::plain(name);
        ToolRouterIndex::build(
            &[ConfiguredToolSpec::new(function_tool(name), false)],
            &ToolRegistry::with_handler_for_test(tool_name, Arc::new(TestHandler)),
            &Default::default(),
        )
    }

    fn function_tool(name: &str) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: name.to_string(),
            description: String::new(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(Default::default(), None, Some(false.into())),
            output_schema: None,
        })
    }
}
