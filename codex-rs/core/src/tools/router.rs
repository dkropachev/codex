use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutputTokenAccounting;
use crate::tools::context::ToolPayload;
use crate::tools::registry::AnyToolResult;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::registry::ToolRegistry;
use crate::tools::router_index::ToolRouterIndex;
use crate::tools::routing_tool;
use crate::tools::routing_tool::RouterResolution;
use crate::tools::spec::build_specs_with_discoverable_tools;
use codex_mcp::ToolInfo;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::SearchToolCallParams;
use codex_protocol::models::ShellToolCallParams;
use codex_state::ToolRouterGuidanceKey;
use codex_state::ToolRouterLedgerEntry;
use codex_state::ToolRouterRulePruneOptions;
use codex_tools::ConfiguredToolSpec;
use codex_tools::DiscoverableTool;
use codex_tools::ResponsesApiNamespaceTool;
use codex_tools::TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP;
use codex_tools::TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION;
use codex_tools::TOOL_ROUTER_SCHEMA_VERSION;
use codex_tools::TOOL_ROUTER_TOOL_NAME;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use codex_tools::ToolsConfig;
use codex_tools::compose_tool_router_guidance;
use codex_tools::create_tool_router_tool;
use codex_tools::estimate_router_text_tokens;
use codex_tools::tool_router_format_description;
use codex_tools::toolset_hash_from_specs;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::instrument;

pub use crate::tools::context::ToolCallSource;

const TOOL_ROUTER_RULE_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const TOOL_ROUTER_RULE_MAX_COUNT: i64 = 1_000;

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub tool_name: ToolName,
    pub call_id: String,
    pub payload: ToolPayload,
}

pub struct ToolRouter {
    registry: ToolRegistry,
    specs: Vec<ConfiguredToolSpec>,
    index: ToolRouterIndex,
    model_visible_specs: Vec<ToolSpec>,
    parallel_mcp_server_names: HashSet<String>,
    tool_router_token_estimates: Option<ToolRouterTokenEstimates>,
    tool_router_prompt_info: Option<ToolRouterPromptInfo>,
}

pub(crate) struct ToolRouterParams<'a> {
    pub(crate) mcp_tools: Option<HashMap<String, ToolInfo>>,
    pub(crate) deferred_mcp_tools: Option<HashMap<String, ToolInfo>>,
    pub(crate) unavailable_called_tools: Vec<ToolName>,
    pub(crate) parallel_mcp_server_names: HashSet<String>,
    pub(crate) discoverable_tools: Option<Vec<DiscoverableTool>>,
    pub(crate) dynamic_tools: &'a [DynamicToolSpec],
}

#[derive(Clone, Copy)]
struct ToolRouterTokenEstimates {
    visible_router_schema_tokens: i64,
    hidden_tool_schema_tokens: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolRouterPromptInfo {
    pub(crate) format_description: String,
    pub(crate) format_description_tokens: i64,
    pub(crate) toolset_hash: String,
    pub(crate) router_schema_version: i64,
}

#[derive(Clone, Copy, Debug)]
struct ToolRouterGuidanceTelemetry {
    guidance_version: i64,
    guidance_tokens: i64,
}

#[derive(Clone)]
struct RoutedDispatchContext {
    session: Arc<Session>,
    turn: Arc<TurnContext>,
    cancellation_token: CancellationToken,
    tracker: SharedTurnDiffTracker,
    router_call_id: String,
}

impl ToolRouter {
    pub fn from_config(config: &ToolsConfig, params: ToolRouterParams<'_>) -> Self {
        let ToolRouterParams {
            mcp_tools,
            deferred_mcp_tools,
            unavailable_called_tools,
            parallel_mcp_server_names,
            discoverable_tools,
            dynamic_tools,
        } = params;
        let builder = build_specs_with_discoverable_tools(
            config,
            mcp_tools,
            deferred_mcp_tools,
            unavailable_called_tools,
            discoverable_tools,
            dynamic_tools,
        );
        let (specs, registry) = builder.build();
        let index = ToolRouterIndex::build(&specs, &registry, &parallel_mcp_server_names);
        let unwrapped_model_visible_specs: Vec<ToolSpec> = if config.code_mode_only_enabled {
            specs
                .iter()
                .filter_map(|configured_tool| {
                    if !codex_code_mode::is_code_mode_nested_tool(configured_tool.name()) {
                        Some(configured_tool.spec.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            specs
                .iter()
                .map(|configured_tool| configured_tool.spec.clone())
                .collect()
        };
        let (model_visible_specs, tool_router_token_estimates, tool_router_prompt_info) =
            if config.tool_router {
                let router_spec = create_tool_router_tool();
                let format_description =
                    tool_router_format_description(&router_spec, &unwrapped_model_visible_specs);
                let token_estimates = ToolRouterTokenEstimates {
                    visible_router_schema_tokens: estimate_tool_schema_tokens(
                        std::slice::from_ref(&router_spec),
                    ),
                    hidden_tool_schema_tokens: estimate_tool_schema_tokens(
                        &unwrapped_model_visible_specs,
                    ),
                };
                let prompt_info = ToolRouterPromptInfo {
                    format_description_tokens: i64::try_from(estimate_router_text_tokens(
                        &format_description,
                    ))
                    .unwrap_or(i64::MAX),
                    format_description,
                    toolset_hash: toolset_hash_from_specs(&unwrapped_model_visible_specs),
                    router_schema_version: TOOL_ROUTER_SCHEMA_VERSION,
                };
                (vec![router_spec], Some(token_estimates), Some(prompt_info))
            } else {
                (unwrapped_model_visible_specs, None, None)
            };

        Self {
            registry,
            specs,
            index,
            model_visible_specs,
            parallel_mcp_server_names,
            tool_router_token_estimates,
            tool_router_prompt_info,
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.specs
            .iter()
            .map(|config| config.spec.clone())
            .collect()
    }

    pub fn model_visible_specs(&self) -> Vec<ToolSpec> {
        self.model_visible_specs.clone()
    }

    pub(crate) fn tool_router_prompt_info(&self) -> Option<&ToolRouterPromptInfo> {
        self.tool_router_prompt_info.as_ref()
    }

    pub(crate) fn learned_rule_tool_names(&self) -> BTreeSet<String> {
        self.index.learned_rule_tool_names()
    }

    pub fn find_spec(&self, tool_name: &ToolName) -> Option<ToolSpec> {
        self.specs.iter().find_map(|config| match &config.spec {
            ToolSpec::Function(tool)
                if tool_name.namespace.is_none() && tool.name == tool_name.name =>
            {
                Some(config.spec.clone())
            }
            ToolSpec::Freeform(tool)
                if tool_name.namespace.is_none() && tool.name == tool_name.name =>
            {
                Some(config.spec.clone())
            }
            ToolSpec::Namespace(namespace) => namespace.tools.iter().find_map(|tool| match tool {
                ResponsesApiNamespaceTool::Function(tool)
                    if tool_name.namespace.as_deref() == Some(namespace.name.as_str())
                        && tool.name == tool_name.name =>
                {
                    Some(ToolSpec::Function(tool.clone()))
                }
                _ => None,
            }),
            _ => None,
        })
    }

    pub(crate) fn create_diff_consumer(
        &self,
        tool_name: &ToolName,
    ) -> Option<Box<dyn ToolArgumentDiffConsumer>> {
        self.registry.create_diff_consumer(tool_name)
    }

    fn configured_tool_supports_parallel(&self, tool_name: &ToolName) -> bool {
        if tool_name.namespace.is_some() {
            return false;
        }

        self.specs
            .iter()
            .filter(|config| config.supports_parallel_tool_calls)
            .any(|config| match &config.spec {
                ToolSpec::Function(tool) => tool.name == tool_name.name.as_str(),
                ToolSpec::Freeform(tool) => tool.name == tool_name.name.as_str(),
                ToolSpec::Namespace(_)
                | ToolSpec::ToolSearch { .. }
                | ToolSpec::LocalShell {}
                | ToolSpec::ImageGeneration { .. }
                | ToolSpec::WebSearch { .. } => false,
            })
    }

    pub fn tool_supports_parallel(&self, call: &ToolCall) -> bool {
        match &call.payload {
            // MCP parallel support is configured per server, including for deferred
            // tools that may not have a matching spec entry. Use the parsed payload
            // server so similarly named servers/tools cannot collide.
            ToolPayload::Mcp { server, .. } => self.parallel_mcp_server_names.contains(server),
            _ => self.configured_tool_supports_parallel(&call.tool_name),
        }
    }

    fn tool_router_fanout_safe(&self, call: &ToolCall) -> bool {
        self.index.fanout_safe(&call.tool_name)
    }

    #[instrument(level = "trace", skip_all, err)]
    pub async fn build_tool_call(
        session: &Session,
        item: ResponseItem,
    ) -> Result<Option<ToolCall>, FunctionCallError> {
        match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                let tool_name = ToolName::new(namespace, name);
                if let Some(tool_info) = session.resolve_mcp_tool_info(&tool_name).await {
                    Ok(Some(ToolCall {
                        tool_name: tool_info.canonical_tool_name(),
                        call_id,
                        payload: ToolPayload::Mcp {
                            server: tool_info.server_name,
                            tool: tool_info.tool.name.to_string(),
                            raw_arguments: arguments,
                        },
                    }))
                } else {
                    Ok(Some(ToolCall {
                        tool_name,
                        call_id,
                        payload: ToolPayload::Function { arguments },
                    }))
                }
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
            ResponseItem::LocalShellCall {
                id,
                call_id,
                action,
                ..
            } => {
                let call_id = call_id
                    .or(id)
                    .ok_or(FunctionCallError::MissingLocalShellCallId)?;

                match action {
                    LocalShellAction::Exec(exec) => {
                        let params = ShellToolCallParams {
                            command: exec.command,
                            workdir: exec.working_directory,
                            timeout_ms: exec.timeout_ms,
                            sandbox_permissions: Some(SandboxPermissions::UseDefault),
                            additional_permissions: None,
                            prefix_rule: None,
                            justification: None,
                        };
                        Ok(Some(ToolCall {
                            tool_name: ToolName::plain("local_shell"),
                            call_id,
                            payload: ToolPayload::LocalShell { params },
                        }))
                    }
                }
            }
            _ => Ok(None),
        }
    }

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
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;

        if tool_name.namespace.is_none() && tool_name.name == TOOL_ROUTER_TOOL_NAME {
            return self
                .dispatch_tool_router_call(
                    session,
                    turn,
                    cancellation_token,
                    tracker,
                    call_id,
                    payload,
                )
                .await;
        }

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

        self.registry.dispatch_any(invocation).await
    }

    async fn dispatch_tool_router_call(
        &self,
        session: Arc<Session>,
        turn: Arc<TurnContext>,
        cancellation_token: CancellationToken,
        tracker: SharedTurnDiffTracker,
        call_id: String,
        payload: ToolPayload,
    ) -> Result<AnyToolResult, FunctionCallError> {
        self.prune_tool_router_rules(&session).await;
        let arguments = match &payload {
            ToolPayload::Function { arguments } => arguments.clone(),
            ToolPayload::ToolSearch { .. }
            | ToolPayload::Custom { .. }
            | ToolPayload::LocalShell { .. }
            | ToolPayload::Mcp { .. } => {
                self.record_tool_router_error(
                    &session,
                    &turn,
                    &call_id,
                    "invalid_payload",
                    /*request_shape_json*/ None,
                )
                .await;
                return Err(FunctionCallError::RespondToModel(
                    "tool_router expects a function-call JSON payload".to_string(),
                ));
            }
        };
        let request_shape_json =
            routing_tool::sanitized_request_shape_json_from_arguments(&arguments);
        let resolution = match routing_tool::resolve_router_request(
            session.as_ref(),
            &self.index,
            call_id.clone(),
            arguments,
        )
        .await
        {
            Ok(resolution) => resolution,
            Err(err) => {
                self.record_tool_router_error(
                    &session,
                    &turn,
                    &call_id,
                    "route_error",
                    request_shape_json,
                )
                .await;
                return Err(err);
            }
        };

        match resolution {
            RouterResolution::Noop { message, usage } => {
                let output = FunctionToolOutput::from_text(message.clone(), Some(true));
                self.record_tool_router_usage(
                    &session,
                    &turn,
                    &call_id,
                    &usage,
                    ToolOutputTokenAccounting::from_returned(estimate_text_tokens(&message)),
                    Some("noop".to_string()),
                )
                .await;
                Ok(AnyToolResult {
                    call_id,
                    payload,
                    result: Box::new(output),
                    post_tool_use_payload: None,
                })
            }
            RouterResolution::InlineOutput {
                message,
                success,
                usage,
            } => {
                let output = FunctionToolOutput::from_text(message.clone(), Some(success));
                self.record_tool_router_usage(
                    &session,
                    &turn,
                    &call_id,
                    &usage,
                    ToolOutputTokenAccounting::from_returned(estimate_text_tokens(&message)),
                    Some(if success { "ok" } else { "failed" }.to_string()),
                )
                .await;
                Ok(AnyToolResult {
                    call_id,
                    payload,
                    result: Box::new(output),
                    post_tool_use_payload: None,
                })
            }
            RouterResolution::SingleTool { call, usage } => {
                let context = RoutedDispatchContext {
                    session,
                    turn,
                    cancellation_token,
                    tracker,
                    router_call_id: call_id,
                };
                self.dispatch_single_routed_tool(context, payload, *call, usage)
                    .await
            }
            RouterResolution::FanOut { calls, usage } => {
                let context = RoutedDispatchContext {
                    session,
                    turn,
                    cancellation_token,
                    tracker,
                    router_call_id: call_id,
                };
                self.dispatch_fanout_routed_tools(context, payload, calls, usage)
                    .await
            }
        }
    }

    async fn dispatch_single_routed_tool(
        &self,
        context: RoutedDispatchContext,
        payload: ToolPayload,
        call: ToolCall,
        usage: routing_tool::ToolRouterUsage,
    ) -> Result<AnyToolResult, FunctionCallError> {
        let result = self
            .dispatch_routed_inner_tool(context.clone(), call)
            .await?;
        let success = result.result.success_for_logging();
        let response = result
            .result
            .to_response_item(&context.router_call_id, &payload);
        let content = routing_tool::response_to_content_items(response);
        let returned_text =
            codex_protocol::models::function_call_output_content_items_to_text(&content)
                .unwrap_or_default();
        let returned_output_tokens = estimate_text_tokens(&returned_text);
        let token_accounting = result.result.token_accounting(returned_output_tokens);
        let output = FunctionToolOutput::from_content(content, Some(success));
        self.record_tool_router_usage(
            &context.session,
            &context.turn,
            &context.router_call_id,
            &usage,
            token_accounting,
            Some(if success { "ok" } else { "failed" }.to_string()),
        )
        .await;
        Ok(AnyToolResult {
            call_id: context.router_call_id,
            payload,
            result: Box::new(output),
            post_tool_use_payload: None,
        })
    }

    async fn dispatch_fanout_routed_tools(
        &self,
        context: RoutedDispatchContext,
        payload: ToolPayload,
        calls: Vec<ToolCall>,
        usage: routing_tool::ToolRouterUsage,
    ) -> Result<AnyToolResult, FunctionCallError> {
        for call in &calls {
            if !self.tool_router_fanout_safe(call) {
                return Err(FunctionCallError::RespondToModel(format!(
                    "tool_router fanout rejected non-parallel-safe tool `{}`",
                    call.tool_name.display()
                )));
            }
        }

        let mut content = Vec::new();
        let mut all_success = true;
        let mut token_accounting = ToolOutputTokenAccounting::zero();
        for call in calls {
            let label = call.tool_name.display();
            let result = self
                .dispatch_routed_inner_tool(context.clone(), call)
                .await?;
            all_success &= result.result.success_for_logging();
            let response = result
                .result
                .to_response_item(&context.router_call_id, &payload);
            let text = codex_protocol::models::function_call_output_content_items_to_text(
                &routing_tool::response_to_content_items(response),
            )
            .unwrap_or_default();
            token_accounting = token_accounting.saturating_add(
                result
                    .result
                    .token_accounting(estimate_text_tokens(&text)),
            );
            content.push(FunctionCallOutputContentItem::InputText {
                text: format!("## {label}\n{text}"),
            });
        }

        let returned_text =
            codex_protocol::models::function_call_output_content_items_to_text(&content)
                .unwrap_or_default();
        let returned_output_tokens = estimate_text_tokens(&returned_text);
        let token_accounting =
            token_accounting.with_returned_output_tokens(returned_output_tokens);
        let output = FunctionToolOutput::from_content(content, Some(all_success));
        self.record_tool_router_usage(
            &context.session,
            &context.turn,
            &context.router_call_id,
            &usage,
            token_accounting,
            Some(if all_success { "ok" } else { "failed" }.to_string()),
        )
        .await;
        Ok(AnyToolResult {
            call_id: context.router_call_id,
            payload,
            result: Box::new(output),
            post_tool_use_payload: None,
        })
    }

    async fn dispatch_routed_inner_tool(
        &self,
        context: RoutedDispatchContext,
        call: ToolCall,
    ) -> Result<AnyToolResult, FunctionCallError> {
        let ToolCall {
            tool_name,
            call_id,
            payload,
        } = call;
        let invocation = ToolInvocation {
            session: context.session,
            turn: context.turn,
            cancellation_token: context.cancellation_token,
            tracker: context.tracker,
            call_id,
            tool_name,
            source: ToolCallSource::Routed {
                router_call_id: context.router_call_id,
            },
            payload,
        };
        self.registry.dispatch_any(invocation).await
    }

    async fn record_tool_router_usage(
        &self,
        session: &Session,
        turn: &TurnContext,
        call_id: &str,
        usage: &routing_tool::ToolRouterUsage,
        token_accounting: ToolOutputTokenAccounting,
        outcome: Option<String>,
    ) {
        let Some(tokens) = self.tool_router_token_estimates else {
            return;
        };
        let Some(state_db) = session.services.state_db.as_deref() else {
            return;
        };
        let guidance = self.tool_router_guidance_telemetry(session, turn).await;
        let prompt_info = self.tool_router_prompt_info.as_ref();
        if let Err(err) = state_db
            .record_tool_router_ledger_entry(ToolRouterLedgerEntry {
                thread_id: session.conversation_id.to_string(),
                turn_id: turn.sub_id.clone(),
                call_id: call_id.to_string(),
                model_slug: turn.model_info.slug.clone(),
                model_provider: turn.config.model_provider_id.clone(),
                toolset_hash: prompt_info
                    .map(|info| info.toolset_hash.clone())
                    .unwrap_or_default(),
                router_schema_version: prompt_info
                    .map(|info| info.router_schema_version)
                    .unwrap_or_default(),
                model_response_ordinal: turn.model_response_ordinal(),
                guidance_version: guidance.guidance_version,
                guidance_tokens: guidance.guidance_tokens,
                format_description_tokens: prompt_info
                    .map(|info| info.format_description_tokens)
                    .unwrap_or_default(),
                route_kind: usage.route_kind.clone(),
                selected_tools: usage.selected_tools.clone(),
                visible_router_schema_tokens: tokens.visible_router_schema_tokens,
                hidden_tool_schema_tokens: tokens.hidden_tool_schema_tokens,
                spark_prompt_tokens: usage.model_router_prompt_tokens,
                spark_completion_tokens: usage.model_router_completion_tokens,
                fanout_call_count: usage.fanout_call_count,
                returned_output_tokens: token_accounting.returned_output_tokens,
                original_output_tokens: token_accounting.original_output_tokens,
                truncated_output_tokens: token_accounting.truncated_output_tokens,
                outcome,
                request_shape_json: usage.request_shape_json.clone(),
            })
            .await
        {
            tracing::warn!("failed to record tool_router ledger entry: {err}");
        }
    }

    async fn tool_router_guidance_telemetry(
        &self,
        session: &Session,
        turn: &TurnContext,
    ) -> ToolRouterGuidanceTelemetry {
        let Some(prompt_info) = self.tool_router_prompt_info.as_ref() else {
            return ToolRouterGuidanceTelemetry {
                guidance_version: 0,
                guidance_tokens: 0,
            };
        };
        let Some(state_db) = session.services.state_db.as_deref() else {
            return default_tool_router_guidance_telemetry();
        };
        let key = ToolRouterGuidanceKey {
            model_slug: turn.model_info.slug.clone(),
            model_provider: turn.config.model_provider_id.clone(),
            toolset_hash: prompt_info.toolset_hash.clone(),
            router_schema_version: prompt_info.router_schema_version,
        };
        let dynamic_guidance = match state_db.lookup_tool_router_guidance(&key).await {
            Ok(record) => record,
            Err(err) => {
                tracing::warn!("failed to read tool_router guidance: {err}");
                None
            }
        };
        let dynamic_text = dynamic_guidance
            .as_ref()
            .map(|record| record.guidance_text.as_str());
        let composed =
            compose_tool_router_guidance(dynamic_text, TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP);
        ToolRouterGuidanceTelemetry {
            guidance_version: if composed.dynamic_guidance_accepted {
                dynamic_guidance
                    .map(|record| record.guidance_version)
                    .unwrap_or(TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION)
            } else {
                TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION
            },
            guidance_tokens: i64::try_from(composed.tokens).unwrap_or(i64::MAX),
        }
    }

    async fn prune_tool_router_rules(&self, session: &Session) {
        let Some(state_db) = session.services.state_db.as_deref() else {
            return;
        };
        if let Err(err) = state_db
            .prune_tool_router_rules(ToolRouterRulePruneOptions {
                valid_tools: self.learned_rule_tool_names(),
                max_rule_age_ms: TOOL_ROUTER_RULE_MAX_AGE_MS,
                max_rule_count: TOOL_ROUTER_RULE_MAX_COUNT,
            })
            .await
        {
            tracing::warn!("failed to prune tool_router learned rules: {err}");
        }
    }

    async fn record_tool_router_error(
        &self,
        session: &Session,
        turn: &TurnContext,
        call_id: &str,
        outcome: &str,
        request_shape_json: Option<String>,
    ) {
        self.record_tool_router_usage(
            session,
            turn,
            call_id,
            &routing_tool::ToolRouterUsage {
                route_kind: "error".to_string(),
                selected_tools: Vec::new(),
                model_router_prompt_tokens: 0,
                model_router_completion_tokens: 0,
                fanout_call_count: 0,
                request_shape_json,
            },
            ToolOutputTokenAccounting::zero(),
            Some(outcome.to_string()),
        )
        .await;
    }
}
#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;

fn estimate_tool_schema_tokens(tools: &[ToolSpec]) -> i64 {
    let serialized = serde_json::to_string(tools).unwrap_or_default();
    estimate_text_tokens(&serialized)
}

fn default_tool_router_guidance_telemetry() -> ToolRouterGuidanceTelemetry {
    let composed = compose_tool_router_guidance(
        /*dynamic_guidance*/ None,
        TOOL_ROUTER_DEFAULT_GUIDANCE_TOKEN_CAP,
    );
    ToolRouterGuidanceTelemetry {
        guidance_version: TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION,
        guidance_tokens: i64::try_from(composed.tokens).unwrap_or(i64::MAX),
    }
}

fn estimate_text_tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}
