use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::flat_tool_name;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::registry::ToolRegistry;
use crate::tools::spec_plan::build_tool_router;
use codex_features::Feature;
use codex_mcp::ToolInfo;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::SearchToolCallParams;
use codex_state::TOOL_ROUTER_REMEMBERED_TOOL_NAMESPACE_SENTINEL;
use codex_state::ToolRouterLedgerEntry;
use codex_state::ToolRouterRememberedToolKey;
use codex_tools::DiscoverableTool;
use codex_tools::ToolCall as ExtensionToolCall;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::json;
use sha1::Digest;
use sha1::Sha1;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio_util::sync::CancellationToken;
use tracing::instrument;
use tracing::warn;

pub use crate::tools::context::ToolCallSource;

const TOOL_ROUTER_SCHEMA_VERSION: i64 = 1;

struct DirectToolDiagnostics {
    state_db: crate::StateDbHandle,
    ledger_entry: ToolRouterLedgerEntry,
    remembered_tool: ToolRouterRememberedToolKey,
}

struct DirectToolDiagnosticsInput<'a> {
    session: &'a Session,
    turn: &'a TurnContext,
    call_id: &'a str,
    tool_name: &'a ToolName,
    payload: &'a ToolPayload,
    source: &'a ToolCallSource,
    result: &'a Result<AnyToolResult, FunctionCallError>,
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub tool_name: ToolName,
    pub call_id: String,
    pub payload: ToolPayload,
}

pub struct ToolRouter {
    registry: ToolRegistry,
    model_visible_specs: Vec<ToolSpec>,
    toolset_hash: String,
    visible_router_schema_tokens: i64,
}

pub(crate) struct ToolRouterParams<'a> {
    pub(crate) mcp_tools: Option<Vec<ToolInfo>>,
    pub(crate) deferred_mcp_tools: Option<Vec<ToolInfo>>,
    pub(crate) discoverable_tools: Option<Vec<DiscoverableTool>>,
    pub(crate) extension_tool_executors: Vec<Arc<dyn ToolExecutor<ExtensionToolCall>>>,
    pub(crate) dynamic_tools: &'a [DynamicToolSpec],
}

impl ToolRouter {
    pub fn from_turn_context(turn_context: &TurnContext, params: ToolRouterParams<'_>) -> Self {
        build_tool_router(turn_context, params)
    }

    pub(crate) fn from_parts(registry: ToolRegistry, model_visible_specs: Vec<ToolSpec>) -> Self {
        let toolset_json = serde_json::to_string(&model_visible_specs).unwrap_or_default();
        Self {
            registry,
            toolset_hash: toolset_hash(toolset_json.as_bytes()),
            visible_router_schema_tokens: estimate_text_tokens(toolset_json.as_str()),
            model_visible_specs,
        }
    }

    pub fn model_visible_specs(&self) -> Vec<ToolSpec> {
        self.model_visible_specs.clone()
    }

    #[cfg(test)]
    pub(crate) fn registered_tool_names_for_test(&self) -> Vec<ToolName> {
        self.registry.tool_names_for_test()
    }

    #[cfg(test)]
    pub(crate) fn tool_exposure_for_test(
        &self,
        name: &ToolName,
    ) -> Option<crate::tools::registry::ToolExposure> {
        self.registry.tool_exposure(name)
    }

    pub(crate) fn create_diff_consumer(
        &self,
        tool_name: &ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>> {
        self.registry.create_diff_consumer(tool_name)
    }

    pub fn tool_supports_parallel(&self, call: &ToolCall) -> bool {
        self.registry
            .supports_parallel_tool_calls(&call.tool_name)
            .unwrap_or(false)
    }

    pub fn tool_waits_for_runtime_cancellation(&self, call: &ToolCall) -> bool {
        self.registry
            .waits_for_runtime_cancellation(&call.tool_name)
            .unwrap_or(false)
    }

    #[instrument(level = "trace", skip_all, err)]
    pub fn build_tool_call(item: ResponseItem) -> Result<Option<ToolCall>, FunctionCallError> {
        match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                let tool_name = ToolName::new(namespace, name);
                Ok(Some(ToolCall {
                    tool_name,
                    call_id,
                    payload: ToolPayload::Function { arguments },
                }))
            }
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                execution,
                arguments,
                ..
            } if execution == "client" => {
                let arguments: SearchToolCallParams =
                    serde_json::from_value(arguments).map_err(|err| {
                        FunctionCallError::RespondToModel(format!(
                            "failed to parse tool_search arguments: {err}"
                        ))
                    })?;
                Ok(Some(ToolCall {
                    tool_name: ToolName::plain("tool_search"),
                    call_id,
                    payload: ToolPayload::ToolSearch { arguments },
                }))
            }
            ResponseItem::ToolSearchCall { .. } => Ok(None),
            ResponseItem::CustomToolCall {
                name,
                input,
                call_id,
                ..
            } => Ok(Some(ToolCall {
                tool_name: ToolName::plain(name),
                call_id,
                payload: ToolPayload::Custom { input },
            })),
            _ => Ok(None),
        }
    }

    #[allow(dead_code)]
    #[instrument(level = "trace", skip_all, err)]
    pub async fn dispatch_tool_call_with_code_mode_result(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
    ) -> Result<AnyToolResult, FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            turn,
            cancellation_token,
            tracker,
            call,
            source,
            /*terminal_outcome_reached*/ None,
        )
        .await
    }

    #[instrument(level = "trace", skip_all, err)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_tool_call_with_terminal_outcome(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        terminal_outcome_reached: Arc<AtomicBool>,
    ) -> Result<AnyToolResult, FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            turn,
            cancellation_token,
            tracker,
            call,
            source,
            Some(terminal_outcome_reached),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_tool_call_with_code_mode_result_inner(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        terminal_outcome_reached: Option<Arc<AtomicBool>>,
    ) -> Result<AnyToolResult, FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;
        let session_for_diagnostics = Arc::clone(&session);
        let turn_for_diagnostics = Arc::clone(&turn);
        let source_for_diagnostics = source.clone();
        let call_id_for_diagnostics = call_id.clone();
        let tool_name_for_diagnostics = tool_name.clone();
        let payload_for_diagnostics = payload.clone();

        let invocation = ToolInvocation {
            session,
            turn,
            cancellation_token,
            tracker,
            call_id,
            tool_name,
            source,
            payload,
        };

        let result = self
            .registry
            .dispatch_any_with_terminal_outcome(invocation, terminal_outcome_reached)
            .await;

        let diagnostics_input = DirectToolDiagnosticsInput {
            session: &session_for_diagnostics,
            turn: &turn_for_diagnostics,
            call_id: &call_id_for_diagnostics,
            tool_name: &tool_name_for_diagnostics,
            payload: &payload_for_diagnostics,
            source: &source_for_diagnostics,
            result: &result,
        };
        if let Some(diagnostics) = self.build_direct_tool_diagnostics(diagnostics_input) {
            record_direct_tool_diagnostics(diagnostics).await;
        }

        result
    }

    fn build_direct_tool_diagnostics(
        &self,
        input: DirectToolDiagnosticsInput<'_>,
    ) -> Option<DirectToolDiagnostics> {
        let DirectToolDiagnosticsInput {
            session,
            turn,
            call_id,
            tool_name,
            payload,
            source,
            result,
        } = input;
        if !turn.features.enabled(Feature::ToolRouter) {
            return None;
        }

        let state_db = session.state_db()?;

        let input_json = tool_payload_json(payload);
        let output_json = tool_result_json(result);
        let output_tokens = output_json
            .as_deref()
            .map(estimate_text_tokens)
            .unwrap_or_default();
        let tool_success = result
            .as_ref()
            .ok()
            .map(|tool_result| tool_result.result.success_for_logging());
        let outcome = match tool_success {
            Some(true) => Some("ok".to_string()),
            Some(false) | None => Some("failed".to_string()),
        };
        let flat_tool_name = flat_tool_name(tool_name).into_owned();

        let ledger_entry = ToolRouterLedgerEntry {
            thread_id: session.thread_id.to_string(),
            turn_id: turn.sub_id.clone(),
            call_id: call_id.to_string(),
            model_slug: turn.model_info.slug.clone(),
            model_provider: turn.config.model_provider_id.clone(),
            toolset_hash: self.toolset_hash.clone(),
            router_schema_version: TOOL_ROUTER_SCHEMA_VERSION,
            model_response_ordinal: 0,
            guidance_version: 0,
            guidance_tokens: 0,
            format_description_tokens: 0,
            route_kind: "deterministic".to_string(),
            selected_tools: vec![flat_tool_name],
            visible_router_schema_tokens: self.visible_router_schema_tokens,
            hidden_tool_schema_tokens: 0,
            spark_prompt_tokens: 0,
            spark_completion_tokens: 0,
            fanout_call_count: 1,
            returned_output_tokens: output_tokens,
            original_output_tokens: output_tokens,
            truncated_output_tokens: 0,
            outcome,
            request_shape_json: None,
            tool_call_source: Some(tool_call_source_label(source).to_string()),
            tool_name: Some(tool_name.name.clone()),
            tool_namespace: tool_name.namespace.clone(),
            tool_input_json: input_json,
            tool_output_json: output_json,
            tool_success,
            prompt_json: None,
            previous_prompt_json: None,
            dialog_locator_json: Some(tool_dialog_locator_json(session, turn, call_id, source)),
        };

        let remembered_tool = ToolRouterRememberedToolKey {
            repo_key: {
                #[allow(deprecated)]
                {
                    turn.cwd.display().to_string()
                }
            },
            task_key: "chat.default".to_string(),
            tool_namespace: tool_name
                .namespace
                .clone()
                .unwrap_or_else(|| TOOL_ROUTER_REMEMBERED_TOOL_NAMESPACE_SENTINEL.to_string()),
            tool_name: tool_name.name.clone(),
        };

        Some(DirectToolDiagnostics {
            state_db,
            ledger_entry,
            remembered_tool,
        })
    }
}

async fn record_direct_tool_diagnostics(diagnostics: DirectToolDiagnostics) {
    let state_db = diagnostics.state_db;
    if let Err(err) = state_db
        .record_tool_router_ledger_entry(diagnostics.ledger_entry)
        .await
    {
        warn!("failed to record tool router ledger entry: {err:#}");
    }

    if let Err(err) = state_db
        .upsert_tool_router_remembered_tool(diagnostics.remembered_tool)
        .await
    {
        warn!("failed to remember tool router tool usage: {err:#}");
    }
}

fn toolset_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn estimate_text_tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}

fn tool_payload_json(payload: &ToolPayload) -> Option<String> {
    match payload {
        ToolPayload::Function { arguments } => Some(arguments.clone()),
        ToolPayload::ToolSearch { arguments } => serde_json::to_string(arguments).ok(),
        ToolPayload::Custom { input } => Some(input.clone()),
    }
}

fn tool_result_json(result: &Result<AnyToolResult, FunctionCallError>) -> Option<String> {
    match result {
        Ok(result) => {
            let response_item = result
                .result
                .to_response_item(&result.call_id, &result.payload);
            serde_json::to_string(&response_item).ok()
        }
        Err(err) => serde_json::to_string(&json!({
            "error": err.to_string(),
        }))
        .ok(),
    }
}

fn tool_call_source_label(source: &ToolCallSource) -> &'static str {
    match source {
        ToolCallSource::Direct => "direct",
        ToolCallSource::CodeMode { .. } => "code_mode",
    }
}

fn tool_dialog_locator_json(
    session: &Session,
    turn: &TurnContext,
    call_id: &str,
    source: &ToolCallSource,
) -> String {
    let mut locator = json!({
        "threadId": session.thread_id.to_string(),
        "turnId": turn.sub_id.as_str(),
        "callId": call_id,
        "source": tool_call_source_label(source),
    });
    if let ToolCallSource::CodeMode {
        cell_id,
        runtime_tool_call_id,
    } = source
        && let Some(object) = locator.as_object_mut()
    {
        object.insert("cellId".to_string(), json!(cell_id));
        object.insert("runtimeToolCallId".to_string(), json!(runtime_tool_call_id));
    }
    locator.to_string()
}

pub(crate) fn extension_tool_executors(
    session: &Session,
) -> Vec<Arc<dyn ToolExecutor<ExtensionToolCall>>> {
    session
        .services
        .extensions
        .tool_contributors()
        .iter()
        .flat_map(|contributor| {
            contributor.tools(
                &session.services.session_extension_data,
                &session.services.thread_extension_data,
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
