use crate::AdditionalProperties;
use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub const TOOL_ROUTER_TOOL_NAME: &str = "tool_router";

pub fn create_tool_router_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "request".to_string(),
            JsonSchema::string(Some("Original user intent for this routed tool request.".to_string())),
        ),
        ("where".to_string(), router_where_schema()),
        (
            "targets".to_string(),
            JsonSchema::array(
                router_target_schema(),
                Some("Typed targets for the request, such as paths, tool names, agents, MCP servers, apps, or queries.".to_string()),
            ),
        ),
        ("action".to_string(), router_action_schema()),
        (
            "verbosity".to_string(),
            JsonSchema::string_enum(
                vec![json!("auto"), json!("brief"), json!("normal"), json!("full")],
                Some("How much output detail to return.".to_string()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: TOOL_ROUTER_TOOL_NAME.to_string(),
        description: "Route one structured request to the appropriate internal Codex tool. Use exact tool names when known; otherwise set where.kind and action.kind with the smallest sufficient payload."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec![
                "request".to_string(),
                "where".to_string(),
                "targets".to_string(),
                "action".to_string(),
                "verbosity".to_string(),
            ]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

fn router_where_schema() -> JsonSchema {
    let properties = BTreeMap::from([
        (
            "kind".to_string(),
            JsonSchema::string_enum(
                vec![
                    json!("none"),
                    json!("workspace"),
                    json!("filesystem"),
                    json!("shell"),
                    json!("git"),
                    json!("mcp"),
                    json!("app"),
                    json!("skill"),
                    json!("web"),
                    json!("image"),
                    json!("agent"),
                    json!("memory"),
                    json!("config"),
                ],
                Some("Tool domain for the request.".to_string()),
            ),
        ),
        (
            "namespace".to_string(),
            JsonSchema::string(Some(
                "Optional internal namespace, such as an MCP namespace.".to_string(),
            )),
        ),
    ]);

    JsonSchema::object(
        properties,
        Some(vec!["kind".to_string()]),
        Some(false.into()),
    )
}

fn router_target_schema() -> JsonSchema {
    let properties = BTreeMap::from([
        (
            "kind".to_string(),
            JsonSchema::string_enum(
                vec![
                    json!("tool"),
                    json!("path"),
                    json!("uri"),
                    json!("agent"),
                    json!("server"),
                    json!("namespace"),
                    json!("query"),
                    json!("text"),
                ],
                Some("Target type.".to_string()),
            ),
        ),
        ("name".to_string(), JsonSchema::string(None)),
        ("id".to_string(), JsonSchema::string(None)),
        ("path".to_string(), JsonSchema::string(None)),
        ("uri".to_string(), JsonSchema::string(None)),
        ("namespace".to_string(), JsonSchema::string(None)),
        ("value".to_string(), JsonSchema::string(None)),
    ]);

    JsonSchema::object(properties, /*required*/ None, Some(false.into()))
}

fn router_action_schema() -> JsonSchema {
    let string_or_string_array = JsonSchema::any_of(
        vec![
            JsonSchema::string(None),
            JsonSchema::array(JsonSchema::string(None), None),
        ],
        None,
    );
    let free_object =
        JsonSchema::object(BTreeMap::new(), /*required*/ None, Some(true.into()));
    let properties = BTreeMap::from([
        (
            "kind".to_string(),
            JsonSchema::string(Some("Action kind, such as exec, git, apply_patch, write_stdin, mcp, spawn_agent, wait_agent, tool_search, view_image, or direct_tool.".to_string())),
        ),
        (
            "description".to_string(),
            JsonSchema::string(Some("Short human-readable action description.".to_string())),
        ),
        ("tool".to_string(), JsonSchema::string(Some("Exact internal tool name when known.".to_string()))),
        ("name".to_string(), JsonSchema::string(Some("Exact action or tool name when known.".to_string()))),
        ("cmd".to_string(), JsonSchema::string(Some("Shell command or script.".to_string()))),
        ("command".to_string(), string_or_string_array),
        ("patch".to_string(), JsonSchema::string(Some("Full apply_patch patch body.".to_string()))),
        ("input".to_string(), free_object.clone()),
        ("query".to_string(), JsonSchema::string(None)),
        ("agent_task".to_string(), JsonSchema::string(None)),
        ("mcp_args".to_string(), free_object),
        ("target".to_string(), JsonSchema::string(None)),
        (
            "targets".to_string(),
            JsonSchema::array(JsonSchema::string(None), None),
        ),
        ("session_id".to_string(), JsonSchema::integer(None)),
        ("chars".to_string(), JsonSchema::string(None)),
        ("workdir".to_string(), JsonSchema::string(None)),
        ("timeout_ms".to_string(), JsonSchema::integer(None)),
        ("yield_time_ms".to_string(), JsonSchema::integer(None)),
        ("max_output_tokens".to_string(), JsonSchema::integer(None)),
        ("sandbox_permissions".to_string(), JsonSchema::string(None)),
        ("justification".to_string(), JsonSchema::string(None)),
        (
            "prefix_rule".to_string(),
            JsonSchema::array(JsonSchema::string(None), None),
        ),
        ("detail".to_string(), JsonSchema::string(None)),
    ]);

    JsonSchema::object(
        properties,
        Some(vec!["kind".to_string(), "description".to_string()]),
        Some(AdditionalProperties::Boolean(true)),
    )
}

#[cfg(test)]
#[path = "tool_router_tests.rs"]
mod tests;
