use crate::function_tool::FunctionCallError;
use crate::responses_metadata::TurnToolNamespacesInfo;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolCallState;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::flat_tool_name;
#[cfg(test)]
use crate::tools::handlers::ToolSearchHandlerCache;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::registry::ToolRegistry;
#[cfg(test)]
use crate::tools::spec_plan::finalize_tool_router;
use codex_features::Feature;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::SearchToolCallParams;
#[cfg(test)]
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ToolMode;
use codex_state::TOOL_ROUTER_REMEMBERED_TOOL_NAMESPACE_SENTINEL;
use codex_state::ToolRouterLedgerEntry;
use codex_state::ToolRouterRememberedToolKey;
use codex_tools::DiscoverableTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde_json::json;
use sha1::Digest;
use sha1::Sha1;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;
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
    step_context: &'a StepContext,
    call_id: &'a str,
    tool_name: &'a ToolName,
    payload: &'a ToolPayload,
    source: &'a ToolCallSource,
    result: &'a Result<AnyToolResult, FunctionCallError>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub tool_name: ToolName,
    pub call_id: String,
    pub payload: ToolPayload,
    pub encrypted_function_args: Option<Vec<String>>,
}

impl ToolCall {
    pub(crate) fn direct_source(&self) -> ToolCallSource {
        if self.tool_name.namespace.as_deref() == Some("collaboration")
            && matches!(
                self.tool_name.name.as_str(),
                "spawn_agent" | "send_message" | "followup_task"
            )
            && self
                .encrypted_function_args
                .as_ref()
                .is_some_and(Vec::is_empty)
        {
            ToolCallSource::DirectPlaintextMessage
        } else {
            ToolCallSource::Direct
        }
    }
}

pub(crate) fn tool_log_payload<'a>(
    payload: &'a ToolPayload,
    source: &ToolCallSource,
) -> Cow<'a, str> {
    if matches!(source, ToolCallSource::DirectPlaintextMessage) {
        return Cow::Borrowed("[plaintext arguments]");
    }
    payload.log_payload()
}

/// One finalized tool plan: its advertised surfaces and matching executable runtimes.
pub struct ToolRouter {
    registry: ToolRegistry,
    model_visible_specs: Arc<[ToolSpec]>,
    toolset_hash: String,
    visible_router_schema_tokens: i64,
    tool_mode: ToolMode,
    code_mode_tool_names: BTreeMap<String, ToolName>,
    tool_namespaces_info: Option<TurnToolNamespacesInfo>,
    can_manage_children: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSuggestPresentation {
    ListTool,
    RecommendationContext,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolSuggestCandidates {
    pub(crate) tools: Vec<DiscoverableTool>,
    pub(crate) presentation: ToolSuggestPresentation,
}

impl ToolRouter {
    #[cfg(test)]
    pub(crate) fn from_registry(
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        registry: ToolRegistry,
        hosted_specs: Vec<ToolSpec>,
        tool_search_handler_cache: &ToolSearchHandlerCache,
    ) -> Self {
        finalize_tool_router(
            turn_context,
            model_info,
            registry,
            hosted_specs,
            tool_search_handler_cache,
        )
        .expect("test tool registry should not contain duplicate tools")
    }

    pub(crate) fn from_parts(
        registry: ToolRegistry,
        model_visible_specs: Vec<ToolSpec>,
        tool_mode: ToolMode,
        code_mode_tool_names: BTreeMap<String, ToolName>,
        tool_namespaces_info: Option<TurnToolNamespacesInfo>,
        child_management_tools: &[ToolName],
    ) -> Self {
        let toolset_json = serde_json::to_string(&model_visible_specs).unwrap_or_default();
        let mut router = Self {
            registry,
            toolset_hash: toolset_hash(toolset_json.as_bytes()),
            visible_router_schema_tokens: estimate_text_tokens(toolset_json.as_str()),
            model_visible_specs: model_visible_specs.into(),
            tool_mode,
            code_mode_tool_names,
            tool_namespaces_info,
            can_manage_children: false,
        };
        router.can_manage_children = !child_management_tools.is_empty()
            && child_management_tools
                .iter()
                .all(|name| router.exposes_tool(name));
        router
    }

    pub(crate) fn model_visible_specs(&self) -> Arc<[ToolSpec]> {
        Arc::clone(&self.model_visible_specs)
    }

    pub(crate) fn tool_mode(&self) -> ToolMode {
        self.tool_mode
    }

    /// Code Mode still needs its dispatcher when the nested tool set is empty.
    pub(crate) fn requires_code_mode_worker(&self) -> bool {
        matches!(self.tool_mode, ToolMode::CodeMode | ToolMode::CodeModeOnly)
    }

    /// The normalized nested identities chosen after exclusions and collisions.
    // Consumed by the follow-up cell-origin migration.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn code_mode_tool_names(&self) -> &BTreeMap<String, ToolName> {
        &self.code_mode_tool_names
    }

    /// Optional request inventory for this exact plan, without publishing it to turn state.
    pub(crate) fn tool_namespaces_info(&self) -> Option<&TurnToolNamespacesInfo> {
        self.tool_namespaces_info.as_ref()
    }

    /// Whether the model can both start and interact with a terminal process.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn has_terminal_controls(&self) -> bool {
        self.exposes_tool(&ToolName::plain("exec_command"))
            && self.exposes_tool(&ToolName::plain("write_stdin"))
    }

    /// Whether the configured collaboration backend's child-management tools remain exposed.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn can_manage_children(&self) -> bool {
        self.can_manage_children
    }

    // Answers if the tool plan lets the model invoke the tool directly, through code mode, or deferred tool search.
    fn exposes_tool(&self, name: &ToolName) -> bool {
        let name = name.clone().with_default_namespace();
        if self
            .code_mode_tool_names
            .values()
            .any(|nested| nested.clone().with_default_namespace() == name)
            || self.model_visible_specs.iter().any(|spec| match spec {
                ToolSpec::Function(_) | ToolSpec::Freeform(_) => {
                    name.is_default_namespace() && spec.name() == name.name
                }
                ToolSpec::Namespace(namespace) => {
                    name.namespace.as_deref() == Some(namespace.name.as_str())
                        && namespace.tools.iter().any(|tool| match tool {
                            ResponsesApiNamespaceTool::Function(tool) => tool.name == name.name,
                            ResponsesApiNamespaceTool::Custom(tool) => tool.name == name.name,
                        })
                }
                ToolSpec::ToolSearch { .. } | ToolSpec::WebSearch { .. } => false,
            })
        {
            return true;
        }
        self.model_visible_specs
            .iter()
            .any(|spec| matches!(spec, ToolSpec::ToolSearch { .. }))
            && self.registry.entries().any(|tool| {
                tool.exposure.is_deferred()
                    && tool.runtime.tool_name().with_default_namespace() == name
                    && (tool.runtime.immutable_spec().is_some()
                        || tool.runtime.search_info().is_some())
            })
    }

    pub(crate) fn deferred_tool_namespaces(&self) -> BTreeMap<String, String> {
        self.registry.deferred_tool_namespaces()
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

    pub(crate) fn tool_runtime(&self, tool_name: &ToolName) -> Option<Arc<dyn CoreToolRuntime>> {
        self.registry.tool(tool_name)
    }

    #[instrument(level = "trace", skip_all, err)]
    pub fn build_tool_call(item: ResponseItem) -> Result<Option<ToolCall>, FunctionCallError> {
        match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                encrypted_function_args,
                call_id,
                ..
            } => {
                let tool_name = ToolName::new(namespace, name).with_default_namespace();
                Ok(Some(ToolCall {
                    tool_name,
                    call_id,
                    payload: ToolPayload::Function { arguments },
                    encrypted_function_args,
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
                    encrypted_function_args: None,
                }))
            }
            ResponseItem::ToolSearchCall { .. } => Ok(None),
            ResponseItem::CustomToolCall {
                name,
                namespace,
                input,
                call_id,
                ..
            } => Ok(Some(ToolCall {
                tool_name: ToolName::new(namespace, name).with_default_namespace(),
                call_id,
                payload: ToolPayload::Custom { input },
                encrypted_function_args: None,
            })),
            _ => Ok(None),
        }
    }

    #[allow(dead_code)]
    #[instrument(level = "trace", skip_all, err)]
    pub async fn dispatch_tool_call_with_code_mode_result(
        &self,
        session: Arc<Session>,
        step_context: Arc<StepContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
    ) -> Result<AnyToolResult, FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            step_context,
            cancellation_token,
            tracker,
            call,
            source,
            /*call_state*/ None,
        )
        .await
    }

    #[instrument(level = "trace", skip_all, err)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn dispatch_tool_call_with_state(
        &self,
        session: Arc<Session>,
        step_context: Arc<StepContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        call_state: Arc<ToolCallState>,
    ) -> Result<AnyToolResult, FunctionCallError> {
        self.dispatch_tool_call_with_code_mode_result_inner(
            session,
            step_context,
            cancellation_token,
            tracker,
            call,
            source,
            Some(call_state),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_tool_call_with_code_mode_result_inner(
        &self,
        session: Arc<Session>,
        step_context: Arc<StepContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call: ToolCall,
        source: ToolCallSource,
        call_state: Option<Arc<ToolCallState>>,
    ) -> Result<AnyToolResult, FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
            ..
        } = call;
        let session_for_diagnostics = Arc::clone(&session);
        let step_context_for_diagnostics = Arc::clone(&step_context);
        let source_for_diagnostics = source.clone();
        let call_id_for_diagnostics = call_id.clone();
        let tool_name_for_diagnostics = tool_name.clone();
        let payload_for_diagnostics = payload.clone();

        // Keep the legacy ToolInvocation.turn field tied to the same request state until handlers migrate.
        let turn = Arc::clone(&step_context.turn);
        let invocation = ToolInvocation {
            session,
            turn,
            step_context,
            cancellation_token,
            tracker,
            call_id,
            tool_name,
            source,
            payload,
        };

        let result = self
            .registry
            .dispatch_any_with_state(invocation, call_state)
            .await;

        let diagnostics_input = DirectToolDiagnosticsInput {
            session: &session_for_diagnostics,
            step_context: &step_context_for_diagnostics,
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
            step_context,
            call_id,
            tool_name,
            payload,
            source,
            result,
        } = input;
        let turn = &step_context.turn;
        if !turn.config.features.get().enabled(Feature::ToolRouter) {
            return None;
        }

        let state_db = session.state_db()?;

        let input_json = tool_payload_json(payload, source);
        let output_json = tool_result_json(result);
        let output_tokens = output_json
            .as_deref()
            .map(estimate_text_tokens)
            .unwrap_or_default();
        let token_usage_hint = result
            .as_ref()
            .ok()
            .map(|tool_result| tool_result.result.token_usage_hint())
            .unwrap_or_default();
        let original_output_tokens = token_usage_hint
            .original_output_tokens
            .and_then(|tokens| i64::try_from(tokens).ok())
            .unwrap_or(output_tokens);
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
            model_slug: step_context.settings.model_info.slug.clone(),
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
            original_output_tokens,
            truncated_output_tokens: 0,
            output_compaction_filter: token_usage_hint.output_compaction_filter,
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
    let ledger_entry = diagnostics.ledger_entry;
    if let Err(err) = state_db
        .record_tool_router_ledger_entry(ledger_entry.clone())
        .await
    {
        warn!("failed to record tool router ledger entry: {err:#}");
    } else if let Err(err) = state_db
        .record_tool_router_output_optimization_observation(ledger_entry)
        .await
    {
        warn!("failed to record tool router output optimization observation: {err:#}");
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

fn tool_payload_json(payload: &ToolPayload, source: &ToolCallSource) -> Option<String> {
    if matches!(source, ToolCallSource::DirectPlaintextMessage) {
        return None;
    }
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
        ToolCallSource::DirectPlaintextMessage => "direct_plaintext_message",
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

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
