use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRootKind {
    Global,
    Project,
    SearchPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowValidationStatus {
    Valid,
    Invalid,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowValidationInfo {
    pub status: WorkflowValidationStatus,
    pub messages: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRootInfo {
    pub kind: WorkflowRootKind,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSummary {
    pub id: String,
    pub command: Option<String>,
    pub title: Option<String>,
    pub user_description: Option<String>,
    pub search_terms: Vec<String>,
    pub root_label: String,
    pub root_kind: WorkflowRootKind,
    pub root_path: PathBuf,
    pub path: PathBuf,
    pub workflow_yaml_path: PathBuf,
    pub mention_target: String,
    pub validation: WorkflowValidationInfo,
    pub repair_mode: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactInfo {
    pub id: String,
    pub path: PathBuf,
    pub dependencies: Vec<String>,
    pub dev_dependencies: Vec<String>,
    pub git_status: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case", export_to = "v2/")]
pub struct WorkflowConfigValues {
    pub search_paths: Vec<PathBuf>,
    pub default_location: String,
    pub repair_mode: String,
    pub max_repair_cycles: u32,
    pub dependency_update_policy: String,
    pub commit_policy: String,
    pub validation_profile: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListResponse {
    pub roots: Vec<WorkflowRootInfo>,
    pub workflows: Vec<WorkflowSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowReadParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub target: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowReadResponse {
    pub workflow: WorkflowSummary,
    pub workflow_yaml: String,
    pub readme: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactParams {
    pub id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactResponse {
    pub impact: WorkflowImpactInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowDevelopParams {
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowEditParams {
    pub id: String,
    pub instruction: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub input: Option<JsonValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowValidateParams {
    pub id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRepairParams {
    pub id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCommandResponse {
    pub message: String,
    pub data: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowProgressNotification {
    pub run_id: String,
    pub thread_id: Option<String>,
    pub message: String,
    pub data: Option<JsonValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowMarkdownResultNotification {
    pub run_id: String,
    pub thread_id: Option<String>,
    pub markdown: String,
}

macro_rules! workflow_command_response_type {
    ($name:ident) => {
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $name {
            pub message: String,
            pub data: JsonValue,
        }

        impl From<WorkflowCommandResponse> for $name {
            fn from(response: WorkflowCommandResponse) -> Self {
                Self {
                    message: response.message,
                    data: response.data,
                }
            }
        }
    };
}

workflow_command_response_type!(WorkflowDevelopResponse);
workflow_command_response_type!(WorkflowEditResponse);
workflow_command_response_type!(WorkflowRunResponse);
workflow_command_response_type!(WorkflowValidateResponse);
workflow_command_response_type!(WorkflowRepairResponse);
workflow_command_response_type!(WorkflowCommandExecuteResponse);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigReadResponse {
    pub config: WorkflowConfigValues,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigWriteParams {
    pub key: String,
    #[ts(optional = nullable)]
    pub value: Option<JsonValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigWriteResponse {
    pub config: WorkflowConfigValues,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCommandExecuteParams {
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthoringContextPrepareParams {
    #[ts(optional = nullable)]
    pub id: Option<String>,
    #[ts(optional = nullable)]
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthoringContextPrepareResponse {
    pub roots: Vec<WorkflowRootInfo>,
    pub workflows: Vec<WorkflowSummary>,
    pub config: WorkflowConfigValues,
}
