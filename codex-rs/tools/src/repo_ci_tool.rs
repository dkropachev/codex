use crate::AdditionalProperties;
use crate::JsonSchema;
use crate::ResponsesApiNamespace;
use crate::ResponsesApiNamespaceTool;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub const REPO_CI_NAMESPACE: &str = "repo_ci";
pub const REPO_CI_STATUS_TOOL_NAME: &str = "status";
pub const REPO_CI_LEARN_TOOL_NAME: &str = "learn";
pub const REPO_CI_RUN_TOOL_NAME: &str = "run";
pub const REPO_CI_RESULT_TOOL_NAME: &str = "result";

pub fn create_repo_ci_namespace_tool() -> ToolSpec {
    ToolSpec::Namespace(ResponsesApiNamespace {
        name: REPO_CI_NAMESPACE.to_string(),
        description: "Repository CI discovery, verification, cached results, and log artifact access. Use these tools instead of shell commands for regular linting, formatting checks, compiling, building, testing, CI polling, and CI reruns when available. Brief failure responses include bounded error_output; full logs are returned only by detailed result requests."
            .to_string(),
        tools: vec![
            ResponsesApiNamespaceTool::Function(status_tool()),
            ResponsesApiNamespaceTool::Function(learn_tool()),
            ResponsesApiNamespaceTool::Function(run_tool()),
            ResponsesApiNamespaceTool::Function(result_tool()),
        ],
    })
}

pub fn repo_ci_tool_names() -> [&'static str; 4] {
    [
        REPO_CI_STATUS_TOOL_NAME,
        REPO_CI_LEARN_TOOL_NAME,
        REPO_CI_RUN_TOOL_NAME,
        REPO_CI_RESULT_TOOL_NAME,
    ]
}

fn status_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: REPO_CI_STATUS_TOOL_NAME.to_string(),
        description: "Report whether repo-ci has learned this repository, whether learning sources are stale, available modes, validation state, and optional detailed manifest/cache metadata."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([("detail".to_string(), detail_schema())]),
            /*required*/ None,
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn learn_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: REPO_CI_LEARN_TOOL_NAME.to_string(),
        description: "Learn or relearn repository verification commands, write the repo-ci manifest and runner, validate prepare plus fast checks, and return a compact validation result with an artifact_id on failure."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([
                ("detail".to_string(), detail_schema()),
                (
                    "automation".to_string(),
                    JsonSchema::string_enum(
                        vec![json!("local"), json!("remote"), json!("local-and-remote")],
                        Some("Optional automation mode to store in the learned manifest.".to_string()),
                    ),
                ),
                (
                    "local_test_time_budget_sec".to_string(),
                    JsonSchema::integer(Some(
                        "Optional local runner timeout budget in seconds.".to_string(),
                    )),
                ),
            ]),
            /*required*/ None,
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn run_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: REPO_CI_RUN_TOOL_NAME.to_string(),
        description: "Run repo-ci verification using learned repository commands. Defaults to fast mode, learns stale or missing metadata by default, reuses cached passing results by default, and records best-effort CPU/memory usage for the runner and attributed containers. Brief failures return error_output and artifact_id; detailed output may include bounded stdout/stderr."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([
                (
                    "mode".to_string(),
                    JsonSchema::string_enum(
                        vec![json!("prepare"), json!("fast"), json!("full")],
                        Some("Run mode. Defaults to fast.".to_string()),
                    ),
                ),
                ("detail".to_string(), detail_schema()),
                (
                    "reuse".to_string(),
                    JsonSchema::string_enum(
                        vec![json!("auto"), json!("never")],
                        Some("Whether to reuse a cached passing run. Defaults to auto.".to_string()),
                    ),
                ),
                (
                    "learn_if_needed".to_string(),
                    JsonSchema::boolean(Some(
                        "If true, learn or relearn when repo-ci metadata is missing or stale. Defaults to true."
                            .to_string(),
                    )),
                ),
            ]),
            /*required*/ None,
            Some(false.into()),
        ),
        output_schema: None,
    }
}

fn result_tool() -> ResponsesApiTool {
    ResponsesApiTool {
        name: REPO_CI_RESULT_TOOL_NAME.to_string(),
        description: "Read a stored repo-ci run artifact. Brief output returns metadata, step statuses, and resource usage when available; detailed output returns bounded logs for the run or selected step."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            BTreeMap::from([
                (
                    "artifact_id".to_string(),
                    JsonSchema::string(Some("Artifact ID returned by repo_ci.run or repo_ci.learn.".to_string())),
                ),
                ("detail".to_string(), detail_schema()),
                (
                    "step_id".to_string(),
                    JsonSchema::string(Some(
                        "Optional step ID to focus detailed logs on.".to_string(),
                    )),
                ),
                (
                    "tail_lines".to_string(),
                    JsonSchema::integer(Some(
                        "Optional number of trailing log lines to include in detailed output.".to_string(),
                    )),
                ),
                (
                    "max_bytes".to_string(),
                    JsonSchema::integer(Some(
                        "Optional maximum log bytes to include in detailed output.".to_string(),
                    )),
                ),
            ]),
            Some(vec!["artifact_id".to_string()]),
            Some(AdditionalProperties::Boolean(false)),
        ),
        output_schema: None,
    }
}

fn detail_schema() -> JsonSchema {
    JsonSchema::string_enum(
        vec![json!("brief"), json!("detailed")],
        Some("Response detail. Defaults to brief.".to_string()),
    )
}

#[cfg(test)]
#[path = "repo_ci_tool_tests.rs"]
mod tests;
