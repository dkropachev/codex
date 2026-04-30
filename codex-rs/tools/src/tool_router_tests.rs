use pretty_assertions::assert_eq;

use super::*;

#[test]
fn tool_router_schema_contains_required_route_fields() {
    let serialized = serde_json::to_value(create_tool_router_tool()).expect("serialize tool");

    assert_eq!(serialized["type"], "function");
    assert_eq!(serialized["name"], TOOL_ROUTER_TOOL_NAME);
    assert_eq!(
        serialized["parameters"]["required"],
        serde_json::json!(["request", "where", "targets", "action", "verbosity"])
    );
    assert_eq!(
        serialized["parameters"]["properties"]["where"]["properties"]["kind"]["enum"],
        serde_json::json!([
            "none",
            "workspace",
            "filesystem",
            "shell",
            "git",
            "mcp",
            "app",
            "skill",
            "web",
            "image",
            "agent",
            "memory",
            "config"
        ])
    );
}
