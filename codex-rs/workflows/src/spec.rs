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
pub const TYPESCRIPT_WORKFLOW_ENTRYPOINT: &str = "src/workflow.ts";
pub const RUNE_WORKFLOW_ENTRYPOINT: &str = "src/workflow.rn";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRuntimeKind {
    #[default]
    Rune,
    Typescript,
}

impl WorkflowRuntimeKind {
    pub const fn default_entrypoint(self) -> &'static str {
        match self {
            WorkflowRuntimeKind::Rune => RUNE_WORKFLOW_ENTRYPOINT,
            WorkflowRuntimeKind::Typescript => TYPESCRIPT_WORKFLOW_ENTRYPOINT,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRuntimeInfo {
    pub kind: WorkflowRuntimeKind,
    pub entrypoint: String,
}

impl WorkflowRuntimeInfo {
    pub fn new(kind: WorkflowRuntimeKind, entrypoint: Option<String>) -> Self {
        let entrypoint = entrypoint
            .filter(|entrypoint| !entrypoint.trim().is_empty())
            .unwrap_or_else(|| kind.default_entrypoint().to_string());
        Self { kind, entrypoint }
    }

    pub fn legacy_typescript() -> Self {
        Self::new(WorkflowRuntimeKind::Typescript, /*entrypoint*/ None)
    }
}

impl Default for WorkflowRuntimeInfo {
    fn default() -> Self {
        Self::new(WorkflowRuntimeKind::Rune, /*entrypoint*/ None)
    }
}

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
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
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
    pub runtime: Option<WorkflowRuntimeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
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

impl WorkflowSpec {
    pub fn resolved_runtime(&self) -> WorkflowRuntimeInfo {
        self.runtime
            .clone()
            .unwrap_or_else(WorkflowRuntimeInfo::legacy_typescript)
    }
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
    runtime: WorkflowRuntimeKind,
    config: &WorkflowsConfigToml,
) -> WorkflowSpec {
    let command = id
        .split('/')
        .next_back()
        .filter(|command| !command.is_empty() && !command.contains('/'))
        .map(ToString::to_string);
    let command_label = command.as_deref().unwrap_or("<cmd>");
    let repair_mode = config
        .repair_mode
        .clone()
        .unwrap_or_else(|| "full".to_string());
    let mut spec = WorkflowSpec {
        id,
        runtime: Some(WorkflowRuntimeInfo::new(runtime, /*entrypoint*/ None)),
        title: Some(title),
        user_description: Some(user_description),
        search_terms: Vec::new(),
        api: JsonValue::Null,
        usage: json!({
            "summary": format!(
                "Run this workflow with `/{command_label}` or `codex {command_label}`."
            )
        }),
        dependencies: JsonValue::Null,
        validation: json!({
            "profile": config.validation_profile.clone().unwrap_or_else(|| "default".to_string()),
            "commands": runtime_validation_commands(runtime),
            "coverage": {
                "positive": true,
                "negative": true,
                "progress": true,
                "finalResult": true,
                "failureUx": true,
                "load": true,
                "autocomplete": true,
                "recovery": false
            }
        }),
        tool: None,
        command,
        repair: Some(WorkflowRepairSpec {
            mode: Some(repair_mode),
            max_repair_cycles: config.max_repair_cycles,
        }),
        git: json!({
            "commitPolicy": config.commit_policy.clone().unwrap_or_else(|| "auto".to_string())
        }),
    };

    match runtime {
        WorkflowRuntimeKind::Rune => {
            spec.api = json!({
                "callableName": workflow_callable_name_from_id(&spec.id),
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": true
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "ok": { "type": "boolean" },
                        "input": { "type": "object", "additionalProperties": true }
                    },
                    "required": ["ok", "input"],
                    "additionalProperties": false
                },
                "formatSchemas": {
                    "tui.markdown.v1": {
                        "type": "object",
                        "properties": {
                            "markdown": { "type": "string" }
                        },
                        "required": ["markdown"],
                        "additionalProperties": false
                    }
                }
            });
        }
        WorkflowRuntimeKind::Typescript => {
            spec.dependencies = json!({
                "runtime": ["@openai/codex-sdk"],
                "development": ["typescript", "tsx", "@types/node"]
            });
            spec.validation["contractSmoke"] = json!({
                "input": { "input": "example" }
            });
        }
    }

    spec
}

fn runtime_validation_commands(runtime: WorkflowRuntimeKind) -> Vec<&'static str> {
    match runtime {
        WorkflowRuntimeKind::Rune => vec!["true"],
        WorkflowRuntimeKind::Typescript => vec!["npm run build", "npm test"],
    }
}

pub fn workflow_callable_name_from_id(id: &str) -> String {
    let mut output = String::new();
    let mut capitalize_next = false;
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            if output.is_empty() {
                output.extend(ch.to_lowercase());
            } else if capitalize_next {
                output.extend(ch.to_uppercase());
                capitalize_next = false;
            } else {
                output.push(ch);
            }
        } else if !output.is_empty() {
            capitalize_next = true;
        }
    }
    if output.is_empty() {
        "workflow".to_string()
    } else {
        output
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
