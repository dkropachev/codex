use codex_experimental_api_macros::ExperimentalApi;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum PromptContextPreset {
    Current,
    Workflow,
    Minimal,
    Isolated,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum PromptBlockMode {
    Inherit,
    Include,
    Omit,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
#[ts(tag = "mode", export_to = "v2/")]
pub enum InstructionPolicy {
    #[schemars(title = "InstructionPolicyInherit")]
    Inherit,
    #[schemars(title = "InstructionPolicyOmit")]
    Omit,
    #[schemars(title = "InstructionPolicySet")]
    Set { text: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct PromptContextPolicy {
    pub preset: Option<PromptContextPreset>,
    pub system_instructions: Option<InstructionPolicy>,
    pub developer: Option<DeveloperPromptPolicy>,
    pub user_context: Option<UserContextPromptPolicy>,
    #[serde(default = "default_true")]
    pub strict: bool,
}

impl Default for PromptContextPolicy {
    fn default() -> Self {
        Self {
            preset: None,
            system_instructions: None,
            developer: None,
            user_context: None,
            strict: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptContextReadParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptContextReadResponse {
    pub system_instructions: String,
    pub developer_instructions: String,
    pub user_instructions: String,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptContextUpdateParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub prompt_context: Option<PromptContextPolicy>,
    #[ts(optional = nullable)]
    pub tool_policy: Option<ToolPolicy>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPromptContextUpdateResponse {}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct DeveloperPromptPolicy {
    pub instructions: Option<InstructionPolicy>,
    pub blocks: Option<DeveloperPromptBlocks>,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct UserContextPromptPolicy {
    pub instructions: Option<InstructionPolicy>,
    pub blocks: Option<UserContextBlocks>,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct DeveloperPromptBlocks {
    pub permissions: Option<PromptBlockMode>,
    pub collaboration_mode: Option<PromptBlockMode>,
    pub memories: Option<PromptBlockMode>,
    pub apps: Option<PromptBlockMode>,
    pub skills: Option<PromptBlockMode>,
    pub plugins: Option<PromptBlockMode>,
    pub commit_attribution: Option<PromptBlockMode>,
    pub personality: Option<PromptBlockMode>,
    pub realtime: Option<PromptBlockMode>,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct UserContextBlocks {
    pub agents_md: Option<PromptBlockMode>,
    pub environment: Option<PromptBlockMode>,
    pub subagents: Option<PromptBlockMode>,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ToolPolicy {
    pub builtins: Option<ToolSetPolicy>,
    pub mcp: Option<McpToolPolicy>,
    pub dynamic: Option<ToolSetPolicy>,
    pub tool_router: Option<ToolRouterPolicy>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
#[ts(tag = "mode", export_to = "v2/")]
pub enum ToolSetPolicy {
    #[schemars(title = "ToolSetPolicyInherit")]
    Inherit,
    #[schemars(title = "ToolSetPolicyNone")]
    None,
    #[schemars(title = "ToolSetPolicyAllowOnly")]
    AllowOnly { tools: Vec<String> },
    #[schemars(title = "ToolSetPolicyDeny")]
    Deny { tools: Vec<String> },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
#[ts(tag = "mode", export_to = "v2/")]
pub enum McpToolPolicy {
    #[schemars(title = "McpToolPolicyInherit")]
    Inherit,
    #[schemars(title = "McpToolPolicyNone")]
    None,
    #[schemars(title = "McpToolPolicyAllowOnly")]
    AllowOnly {
        #[serde(default)]
        servers: Vec<String>,
        #[serde(default)]
        tools: Vec<String>,
    },
    #[schemars(title = "McpToolPolicyDeny")]
    Deny {
        #[serde(default)]
        servers: Vec<String>,
        #[serde(default)]
        tools: Vec<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ToolRouterPolicy {
    Inherit,
    Off,
}
