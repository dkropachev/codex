use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::tools::router::ToolCall;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_deterministic;
use crate::tools::routing_tool;
use crate::tools::routing_tool::RouterArgs;
use crate::tools::routing_tool::RouterResolution;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct LearnedToolCall {
    tool: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum LearnedRoute {
    Route { tool: String, arguments: Value },
    Fanout { calls: Vec<LearnedToolCall> },
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

async fn resolve_learned_route(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    args: &RouterArgs,
    route: LearnedRoute,
) -> Result<RouterResolution, FunctionCallError> {
    match route {
        LearnedRoute::Route { tool, arguments } => {
            let call = call_for_learned_route_tool(
                session,
                index,
                call_id,
                args,
                LearnedToolCall { tool, arguments },
            )
            .await?;
            Ok(routing_tool::route_resolution("learned_rule", call, 0, 0))
        }
        LearnedRoute::Fanout { calls } => {
            let calls =
                calls_for_learned_route_fanout(session, index, call_id.as_str(), args, calls)
                    .await?;
            Ok(routing_tool::fanout_resolution("learned_rule", calls))
        }
    }
}

async fn call_for_learned_route_tool(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: String,
    base_args: &RouterArgs,
    call: LearnedToolCall,
) -> Result<ToolCall, FunctionCallError> {
    let tool_name = index.find_exact(&call.tool, None)?.ok_or_else(|| {
        FunctionCallError::RespondToModel(format!(
            "tool_router learned rule selected unknown tool `{}`",
            call.tool
        ))
    })?;
    if tool_name.name == "tool_router" {
        return Err(FunctionCallError::RespondToModel(
            "tool_router learned rule may not route to tool_router".to_string(),
        ));
    }
    let synthetic_args = synthetic_router_args(base_args, &call)?;
    routing_deterministic::call_for_exact_tool(session, index, call_id, tool_name, &synthetic_args)
        .await
}

async fn calls_for_learned_route_fanout(
    session: &Session,
    index: &ToolRouterIndex,
    call_id: &str,
    base_args: &RouterArgs,
    calls: Vec<LearnedToolCall>,
) -> Result<Vec<ToolCall>, FunctionCallError> {
    if calls.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "tool_router learned rule fanout route requires at least one call".to_string(),
        ));
    }
    let mut routed = Vec::with_capacity(calls.len());
    for (index_value, call) in calls.into_iter().enumerate() {
        let routed_call = call_for_learned_route_tool(
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

fn synthetic_router_args(
    base_args: &RouterArgs,
    call: &LearnedToolCall,
) -> Result<RouterArgs, FunctionCallError> {
    let mut action = Map::new();
    action.insert("kind".to_string(), Value::String("direct_tool".to_string()));
    action.insert(
        "description".to_string(),
        Value::String("learned-rule route".to_string()),
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

fn router_args_from_value(value: Value) -> Result<RouterArgs, FunctionCallError> {
    serde_json::from_value(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("invalid learned tool_router route: {err}"))
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

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
}
