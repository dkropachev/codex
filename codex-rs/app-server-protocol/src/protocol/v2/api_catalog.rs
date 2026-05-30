use super::McpServerStatus;
use super::McpServerStatusDetail;
use super::WorkflowSummary;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ApiCatalogSection {
    AppServerMethods,
    McpServers,
    BuiltInTools,
    WorkflowRuntime,
    Workflows,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogReadParams {
    /// Optional subset of catalog sections to return. Omitted returns every section.
    #[ts(optional = nullable)]
    pub include: Option<Vec<ApiCatalogSection>>,
    /// Controls MCP inventory detail. Defaults to `Full` when omitted.
    #[ts(optional = nullable)]
    pub mcp_detail: Option<McpServerStatusDetail>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogReadResponse {
    pub schema_version: u32,
    pub generated_at: i64,
    pub app_server_methods: Vec<ApiCatalogMethod>,
    pub mcp_servers: Vec<McpServerStatus>,
    pub built_in_tools: Vec<ApiCatalogTool>,
    pub workflow_runtime: ApiCatalogWorkflowRuntime,
    pub workflows: Vec<WorkflowSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogMethod {
    pub method: String,
    pub params_type: String,
    pub response_type: String,
    pub experimental: bool,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ApiCatalogToolSource {
    AppServerRpc,
    WorkflowRuntime,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogTool {
    pub name: String,
    pub source: ApiCatalogToolSource,
    pub invocation: String,
    pub description: String,
    pub input_schema: JsonValue,
    pub output_schema: Option<JsonValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ApiCatalogSymbolKind {
    Function,
    Class,
    Method,
    Type,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogSymbol {
    pub name: String,
    pub kind: ApiCatalogSymbolKind,
    pub signature: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ApiCatalogWorkflowRuntime {
    pub package: String,
    pub import_specifier: String,
    pub symbols: Vec<ApiCatalogSymbol>,
}

pub fn built_in_api_catalog_tools() -> Vec<ApiCatalogTool> {
    vec![
        ApiCatalogTool {
            name: "command/exec".to_string(),
            source: ApiCatalogToolSource::AppServerRpc,
            invocation: "ctx.tools.exec(command, options)".to_string(),
            description: "Run one argv command through the app-server command execution API."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "options": { "type": "object" }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            output_schema: Some(json!({ "type": "object" })),
        },
        ApiCatalogTool {
            name: "mcpServer/tool/call".to_string(),
            source: ApiCatalogToolSource::AppServerRpc,
            invocation: "ctx.mcp.callTool(agentOrThreadId, { server, tool, arguments })"
                .to_string(),
            description: "Call a tool exposed by a configured MCP server for a thread.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "threadId": { "type": "string" },
                    "server": { "type": "string" },
                    "tool": { "type": "string" },
                    "arguments": {},
                    "_meta": {}
                },
                "required": ["threadId", "server", "tool"],
                "additionalProperties": false
            }),
            output_schema: Some(json!({ "type": "object" })),
        },
        ApiCatalogTool {
            name: "defineTool".to_string(),
            source: ApiCatalogToolSource::WorkflowRuntime,
            invocation: "defineTool(spec, handler)".to_string(),
            description:
                "Register a JavaScript dynamic tool that Codex agents can call during a workflow."
                    .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "namespace": { "type": ["string", "null"] },
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "inputSchema": {},
                    "deferLoading": { "type": "boolean" }
                },
                "required": ["name", "description", "inputSchema"],
                "additionalProperties": false
            }),
            output_schema: Some(json!({ "type": "object" })),
        },
    ]
}

pub fn workflow_runtime_api_catalog() -> ApiCatalogWorkflowRuntime {
    ApiCatalogWorkflowRuntime {
        package: "@openai/codex-sdk".to_string(),
        import_specifier: "@openai/codex-sdk/workflow".to_string(),
        symbols: vec![
            symbol(
                "defineWorkflow",
                ApiCatalogSymbolKind::Function,
                "defineWorkflow<Input, Output>(workflow: DefinedWorkflow<Input, Output>): DefinedWorkflow<Input, Output>",
                "Legacy object-style workflow helper retained for compatibility. New workflows should export a named default async function, optional complete(...) autocomplete hook, and optional WorkflowOutput.toTuiMarkdown(result) companion.",
            ),
            symbol(
                "runWorkflow",
                ApiCatalogSymbolKind::Function,
                "runWorkflow<Input, Output>(workflow, options?): Promise<Output>",
                "Run a workflow and return its structured JSON result. Generated workflow wrappers use this internally.",
            ),
            symbol(
                "CodexWorkflow.start",
                ApiCatalogSymbolKind::Method,
                "CodexWorkflow.start(options?): Promise<CodexWorkflow>",
                "Connect to an existing app-server when available, otherwise start a private app-server.",
            ),
            symbol(
                "WorkflowContext.createAgent",
                ApiCatalogSymbolKind::Method,
                "ctx.createAgent(options?): Promise<AgentHandle>",
                "Start an app-server-backed Codex agent thread for this workflow.",
            ),
            symbol(
                "WorkflowContext.resumeAgent",
                ApiCatalogSymbolKind::Method,
                "ctx.resumeAgent(threadId, options?): Promise<AgentHandle>",
                "Resume a persisted Codex thread from workflow code.",
            ),
            symbol(
                "WorkflowContext.progress",
                ApiCatalogSymbolKind::Method,
                "ctx.progress(message: string, data?: unknown): void",
                "Legacy workflow progress helper. Prefer ctx.status(...) for structured workflow and thread status updates.",
            ),
            symbol(
                "WorkflowContext.status",
                ApiCatalogSymbolKind::Method,
                "ctx.status(status: WorkflowStatusUpdate): void",
                "Report structured workflow status to the host and connected clients.",
            ),
            symbol(
                "WorkflowContext.reportToUserMarkdown",
                ApiCatalogSymbolKind::Method,
                "ctx.reportToUserMarkdown(markdown: string): void",
                "Legacy markdown handoff. Prefer WorkflowOutput.toTuiMarkdown(result) for user-facing rendering and use this only as a compatibility escape hatch.",
            ),
            symbol(
                "WorkflowContext.mcp.listServers",
                ApiCatalogSymbolKind::Method,
                "ctx.mcp.listServers({ detail?, cursor?, limit? }): Promise<unknown>",
                "List configured MCP servers, tools, resources, and auth status.",
            ),
            symbol(
                "WorkflowContext.mcp.callTool",
                ApiCatalogSymbolKind::Method,
                "ctx.mcp.callTool(agentOrThreadId, { server, tool, arguments?, meta? }): Promise<unknown>",
                "Call an MCP tool for a workflow agent or thread.",
            ),
            symbol(
                "WorkflowContext.tools.exec",
                ApiCatalogSymbolKind::Method,
                "ctx.tools.exec(command: string[], options?): Promise<unknown>",
                "Run a standalone command through app-server command execution.",
            ),
            symbol(
                "WorkflowContext.api.read",
                ApiCatalogSymbolKind::Method,
                "ctx.api.read(params?): Promise<ApiCatalogReadResponse>",
                "Read this API catalog from workflow code.",
            ),
            symbol(
                "WorkflowContext.artifacts.cache.ensure",
                ApiCatalogSymbolKind::Method,
                "ctx.artifacts.cache.ensure(options): Promise<ArtifactCacheArtifact>",
                "Build or reuse generated workflow artifacts from a content-scoped cache.",
            ),
            symbol(
                "AgentHandle.run",
                ApiCatalogSymbolKind::Method,
                "agent.run(input, options?): Promise<WorkflowTurnResult>",
                "Run a buffered turn and return the final assistant response, turn items, usage, and status.",
            ),
            symbol(
                "AgentHandle.runStreamed",
                ApiCatalogSymbolKind::Method,
                "agent.runStreamed(input, options?): Promise<{ events: AsyncGenerator<AppServerNotification> }>",
                "Run a turn and consume app-server notifications as they arrive.",
            ),
        ],
    }
}

fn symbol(
    name: &str,
    kind: ApiCatalogSymbolKind,
    signature: &str,
    description: &str,
) -> ApiCatalogSymbol {
    ApiCatalogSymbol {
        name: name.to_string(),
        kind,
        signature: signature.to_string(),
        description: description.to_string(),
    }
}
