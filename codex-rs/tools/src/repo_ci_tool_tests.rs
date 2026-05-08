use super::*;
use pretty_assertions::assert_eq;

#[test]
fn repo_ci_namespace_contains_expected_tools() {
    let serialized = serde_json::to_value(create_repo_ci_namespace_tool()).expect("serialize tool");

    assert_eq!(serialized["type"], "namespace");
    assert_eq!(serialized["name"], REPO_CI_NAMESPACE);
    let tools = serialized["tools"]
        .as_array()
        .expect("namespace tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect::<Vec<_>>();

    assert_eq!(tools, repo_ci_tool_names());
    assert_eq!(
        serialized["tools"][2]["parameters"]["properties"]["reuse"]["enum"],
        serde_json::json!(["auto", "never"])
    );
    assert_eq!(
        serialized["tools"][3]["parameters"]["required"],
        serde_json::json!(["artifact_id"])
    );
    assert_eq!(serialized["tools"][4]["name"], "instruction");
    assert_eq!(
        serialized["tools"][4]["parameters"]["properties"]["action"]["enum"],
        serde_json::json!(["show", "set", "clear"])
    );
    assert_eq!(
        serialized["tools"][4]["parameters"]["required"],
        serde_json::json!(["action", "scope"])
    );
}
