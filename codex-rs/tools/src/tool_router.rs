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
        description: "Route one structured request to the appropriate internal Codex tool."
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
                    json!("process"),
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
        ("name".to_string(), JsonSchema::string(/*description*/ None)),
        ("id".to_string(), JsonSchema::string(/*description*/ None)),
        ("path".to_string(), JsonSchema::string(/*description*/ None)),
        ("uri".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "namespace".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
        (
            "value".to_string(),
            JsonSchema::string(/*description*/ None),
        ),
    ]);

    JsonSchema::object(properties, /*required*/ None, Some(false.into()))
}

fn router_action_schema() -> JsonSchema {
    let string_or_string_array = JsonSchema::any_of(
        vec![
            JsonSchema::string(/*description*/ None),
            JsonSchema::array(
                JsonSchema::string(/*description*/ None),
                /*description*/ None,
            ),
        ],
        /*description*/ None,
    );
    let string_array = JsonSchema::array(
        JsonSchema::string(/*description*/ None),
        /*description*/ None,
    );
    let free_object =
        JsonSchema::object(BTreeMap::new(), /*required*/ None, Some(true.into()));
    let properties = BTreeMap::from([
        (
            "kind".to_string(),
            JsonSchema::string(Some("Action kind, such as exec, exec_wait, batch, inspect, git_snapshot, status, git, apply_patch, write_stdin, mcp, spawn_agent, wait_agent, tool_search, view_image, or direct_tool.".to_string())),
        ),
        ("description".to_string(), JsonSchema::string(/*description*/ None)),
        ("tool".to_string(), JsonSchema::string(/*description*/ None)),
        ("name".to_string(), JsonSchema::string(/*description*/ None)),
        ("cmd".to_string(), JsonSchema::string(/*description*/ None)),
        ("command".to_string(), string_or_string_array),
        ("commands".to_string(), JsonSchema::array(JsonSchema::string(/*description*/ None), /*description*/ None)),
        ("paths".to_string(), JsonSchema::array(JsonSchema::string(/*description*/ None), /*description*/ None)),
        ("patch".to_string(), JsonSchema::string(/*description*/ None)),
        ("input".to_string(), free_object.clone()),
        ("query".to_string(), JsonSchema::string(/*description*/ None)),
        ("agent_task".to_string(), JsonSchema::string(/*description*/ None)),
        ("mcp_args".to_string(), free_object),
        ("target".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "targets".to_string(),
            JsonSchema::array(JsonSchema::string(/*description*/ None), /*description*/ None),
        ),
        ("session_id".to_string(), JsonSchema::integer(/*description*/ None)),
        ("chars".to_string(), JsonSchema::string(/*description*/ None)),
        ("workdir".to_string(), JsonSchema::string(/*description*/ None)),
        ("timeout_ms".to_string(), JsonSchema::integer(/*description*/ None)),
        ("wait_until_exit".to_string(), JsonSchema::boolean(/*description*/ None)),
        ("wait_timeout_ms".to_string(), JsonSchema::integer(/*description*/ None)),
        ("yield_time_ms".to_string(), JsonSchema::integer(/*description*/ None)),
        ("max_output_tokens".to_string(), JsonSchema::integer(/*description*/ None)),
        ("sandbox_permissions".to_string(), JsonSchema::string(/*description*/ None)),
        ("justification".to_string(), JsonSchema::string(/*description*/ None)),
        (
            "prefix_rule".to_string(),
            string_array,
        ),
        ("detail".to_string(), JsonSchema::string(/*description*/ None)),
    ]);

    JsonSchema::object(
        properties,
        Some(vec!["kind".to_string()]),
        Some(AdditionalProperties::Boolean(true)),
    )
}

#[cfg(test)]
#[path = "tool_router_tests.rs"]
mod tests;
