use super::shared::v2_enum_from_core;
use codex_protocol::protocol::ImplementMode as CoreImplementMode;
use codex_protocol::protocol::RepoCiIssueType as CoreRepoCiIssueType;
use codex_protocol::protocol::RepoCiSessionMode as CoreRepoCiSessionMode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

v2_enum_from_core! {
    pub enum RepoCiSessionMode from CoreRepoCiSessionMode {
        Off,
        Local,
        Remote,
        LocalAndRemote,
    }
}

v2_enum_from_core! {
    pub enum ImplementMode from CoreImplementMode {
        Auto,
        Implicit,
    }
}

v2_enum_from_core! {
    pub enum RepoCiIssueType from CoreRepoCiIssueType {
        Correctness,
        Reliability,
        Performance,
        Scalability,
        Security,
        Maintainability,
        Testability,
        Observability,
        Compatibility,
        UxConfigCli,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadRepoCiSessionConfigSetParams {
    pub thread_id: String,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub mode: Option<Option<RepoCiSessionMode>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub issue_types: Option<Option<Vec<RepoCiIssueType>>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub review_rounds: Option<Option<u8>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub long_ci: Option<Option<bool>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub implement_enabled: Option<Option<bool>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub implement_mode: Option<Option<ImplementMode>>,
    /// Omit to leave unchanged; null clears the session override and returns to repo/user config.
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub implement_max_cycles: Option<Option<u8>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadRepoCiSessionConfigSetResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadCodexConfigIntentSubmitParams {
    pub thread_id: String,
    pub intent: String,
    #[ts(optional = nullable)]
    pub context: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadCodexConfigIntentSubmitResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiLearningInstructionScopeParams {
    #[ts(optional = nullable)]
    pub cwd: Option<bool>,
    #[ts(optional = nullable)]
    pub github_repo: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiLearningInstructionReadParams {
    pub scope: RepoCiLearningInstructionScopeParams,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiLearningInstructionReadResponse {
    pub scope: String,
    pub instruction: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiLearningInstructionWriteParams {
    pub scope: RepoCiLearningInstructionScopeParams,
    pub instruction: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiLearningInstructionWriteResponse {
    pub scope: String,
    pub old_instruction: Option<String>,
    pub new_instruction: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadModelRouterSessionConfigSetParams {
    pub thread_id: String,
    /// Null clears the session override and returns to repo/user config.
    #[ts(optional = nullable)]
    pub enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadModelRouterSessionConfigSetResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RepoCiStatusNotification {
    pub thread_id: String,
    pub phase: String,
    pub state: String,
    pub scope: String,
    pub attempt: Option<u8>,
    pub max_attempts: Option<u8>,
    pub message: String,
}
