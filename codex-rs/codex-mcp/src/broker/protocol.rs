use std::collections::BTreeMap;
use std::time::Duration;

use codex_rmcp_client::Elicitation;
use codex_rmcp_client::ElicitationResponse;
use codex_rmcp_client::ListToolsWithConnectorIdResult;
use rmcp::model::CallToolResult;
use rmcp::model::InitializeRequestParams;
use rmcp::model::InitializeResult;
use rmcp::model::ListResourceTemplatesResult;
use rmcp::model::ListResourcesResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResult;
use rmcp::model::RequestId;
use serde::Deserialize;
use serde::Serialize;

pub const BROKER_PROTOCOL_VERSION: u32 = 1;
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const METHOD_HELLO: &str = "hello";
pub const METHOD_ACQUIRE: &str = "acquire";
pub const METHOD_RELEASE: &str = "release";
pub const METHOD_LIST_TOOLS: &str = "list_tools";
pub const METHOD_LIST_RESOURCES: &str = "list_resources";
pub const METHOD_LIST_RESOURCE_TEMPLATES: &str = "list_resource_templates";
pub const METHOD_READ_RESOURCE: &str = "read_resource";
pub const METHOD_CALL_TOOL: &str = "call_tool";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReusableServerIdentity {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub placement: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReusableServerLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResponse {
    pub version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireParams {
    pub identity: ReusableServerIdentity,
    pub launch: ReusableServerLaunch,
    pub initialize_params: InitializeRequestParams,
    pub startup_timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireResponse {
    pub lease_id: String,
    pub initialize_result: InitializeResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseParams {
    pub lease_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsParams {
    pub lease_id: String,
    pub params: Option<PaginatedRequestParams>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourcesParams {
    pub lease_id: String,
    pub params: Option<PaginatedRequestParams>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResourceTemplatesParams {
    pub lease_id: String,
    pub params: Option<PaginatedRequestParams>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceParams {
    pub lease_id: String,
    pub params: ReadResourceRequestParams,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolParams {
    pub lease_id: String,
    pub name: String,
    pub arguments: Option<serde_json::Value>,
    pub meta: Option<serde_json::Value>,
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientLine {
    Request {
        id: String,
        method: String,
        params: serde_json::Value,
    },
    ElicitationResponse {
        id: String,
        response: ElicitationClientResponse,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationClientResponse {
    pub result: Option<ElicitationResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerLine {
    Response {
        id: String,
        result: Option<serde_json::Value>,
        error: Option<String>,
    },
    ElicitationRequest {
        id: String,
        request_id: RequestId,
        request: Elicitation,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyResponse {}

pub fn duration_to_millis(duration: Option<Duration>) -> Option<u64> {
    duration.map(|duration| {
        let millis = duration.as_millis();
        u64::try_from(millis).unwrap_or(u64::MAX)
    })
}

pub fn millis_to_duration(millis: Option<u64>) -> Option<Duration> {
    millis.map(Duration::from_millis)
}

pub type ListToolsResponse = ListToolsWithConnectorIdResult;
pub type ListResourcesResponse = ListResourcesResult;
pub type ListResourceTemplatesResponse = ListResourceTemplatesResult;
pub type ReadResourceResponse = ReadResourceResult;
pub type CallToolResponse = CallToolResult;
