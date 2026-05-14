use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use codex_config::types::WorkflowsConfigToml;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;

pub const WORKFLOW_YAML: &str = "workflow.yaml";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowHookKind {
    #[default]
    AfterAgent,
    PreToolUse,
    PostToolUse,
    SessionStart,
    UserPromptSubmit,
    PreCompact,
    PostCompact,
    Stop,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowToolSpec {
    pub description: String,
    pub input_schema: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub output_schema: JsonValue,
    #[serde(
        default = "default_workflow_tool_hooks",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub register_on: Vec<WorkflowHookKind>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSpec {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        alias = "user_description",
        skip_serializing_if = "Option::is_none"
    )]
    pub user_description: Option<String>,
    #[serde(default, alias = "search_terms", skip_serializing_if = "Vec::is_empty")]
    pub search_terms: Vec<String>,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub api: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub usage: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub dependencies: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub validation: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<WorkflowToolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair: Option<WorkflowRepairSpec>,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub git: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRepairSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        default,
        alias = "max_repair_cycles",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_repair_cycles: Option<u32>,
}

pub fn read_workflow_spec(path: &Path) -> Result<WorkflowSpec> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read workflow spec {}", path.display()))?;
    serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse workflow spec {}", path.display()))
}

pub fn write_workflow_spec(path: &Path, spec: &WorkflowSpec) -> Result<()> {
    let contents = serde_yaml::to_string(spec)
        .with_context(|| format!("failed to serialize workflow spec {}", path.display()))?;
    fs::write(path, contents)
        .with_context(|| format!("failed to write workflow spec {}", path.display()))
}

pub fn scaffold_workflow_spec(
    id: String,
    title: String,
    user_description: String,
    config: &WorkflowsConfigToml,
) -> WorkflowSpec {
    let repair_mode = config
        .repair_mode
        .clone()
        .unwrap_or_else(|| "threshold:3".to_string());
    WorkflowSpec {
        id,
        title: Some(title),
        user_description: Some(user_description),
        search_terms: Vec::new(),
        api: json!({
            "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object" }
        }),
        usage: json!({
            "summary": "Run this workflow with `codex workflow run <id> --input '{...}'`."
        }),
        dependencies: json!({
            "runtime": ["@openai/codex-sdk"],
            "development": ["typescript", "tsx", "@types/node"]
        }),
        validation: json!({
            "profile": config.validation_profile.clone().unwrap_or_else(|| "default".to_string()),
            "commands": ["npm run build", "npm test"]
        }),
        tool: None,
        repair: Some(WorkflowRepairSpec {
            mode: Some(repair_mode),
            max_repair_cycles: config.max_repair_cycles,
        }),
        git: json!({
            "commitPolicy": config.commit_policy.clone().unwrap_or_else(|| "auto".to_string())
        }),
    }
}

pub fn workflow_tool_name(id: &str) -> String {
    let mut tool_name = String::from("workflow__");
    for (index, segment) in id.split('/').enumerate() {
        if index > 0 {
            tool_name.push_str("__");
        }
        for ch in segment.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                tool_name.push(ch);
            } else {
                tool_name.push('_');
            }
        }
    }
    if tool_name == "workflow__" {
        "workflow".to_string()
    } else {
        tool_name
    }
}

fn default_workflow_tool_hooks() -> Vec<WorkflowHookKind> {
    vec![WorkflowHookKind::AfterAgent]
}
