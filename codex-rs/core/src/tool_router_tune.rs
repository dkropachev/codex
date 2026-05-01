use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::installation_id::resolve_installation_id;
use crate::model_router::auth_manager_for_config;
use chrono::Utc;
use codex_login::AuthManager;
use codex_models_manager::manager::SharedModelsManager;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_rollout_trace::InferenceTraceContext;
use codex_state::StateRuntime;
use codex_state::ToolRouterDiagnosticsWindow;
use codex_state::ToolRouterGuidanceKey;
use codex_state::ToolRouterGuidanceRecord;
use codex_state::ToolRouterRequestShapeCluster;
use codex_state::ToolRouterTuneCount;
use codex_state::ToolRouterTuneObservation;
use codex_tools::TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION;
use codex_tools::compose_tool_router_guidance;
use codex_tools::tool_router_static_guidelines_tokens;
use codex_tools::validate_tool_router_guidance_cap;
use futures::StreamExt;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;

const DYNAMIC_GUIDANCE_VERSION: i64 = 3;

#[derive(Clone)]
pub struct ToolRouterTuneOptions {
    pub window: String,
    pub model_slug: Option<String>,
    pub max_guidance_tokens: usize,
    pub introspection_model: Option<String>,
    pub introspection_provider: Option<Arc<dyn ToolRouterIntrospectionProvider>>,
    pub apply: bool,
}

impl std::fmt::Debug for ToolRouterTuneOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRouterTuneOptions")
            .field("window", &self.window)
            .field("model_slug", &self.model_slug)
            .field("max_guidance_tokens", &self.max_guidance_tokens)
            .field("introspection_model", &self.introspection_model)
            .field(
                "introspection_provider",
                &self.introspection_provider.as_ref().map(|_| "<provider>"),
            )
            .field("apply", &self.apply)
            .finish()
    }
}

#[async_trait::async_trait]
pub trait ToolRouterIntrospectionProvider: Send + Sync {
    async fn run(
        &self,
        request: ToolRouterIntrospectionRequest,
    ) -> anyhow::Result<ToolRouterIntrospectionRawResponse>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterIntrospectionRequest {
    pub model: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterIntrospectionRawResponse {
    pub output_text: String,
    pub token_usage: ToolRouterTuneTokenUsage,
}

pub struct ToolRouterModelIntrospectionProvider {
    config: Config,
    auth_manager: Arc<AuthManager>,
    models_manager: SharedModelsManager,
    installation_id: String,
}

impl ToolRouterModelIntrospectionProvider {
    pub async fn new(
        config: Config,
        auth_manager: Arc<AuthManager>,
        models_manager: SharedModelsManager,
    ) -> anyhow::Result<Self> {
        let installation_id = resolve_installation_id(&config.codex_home).await?;
        Ok(Self {
            config,
            auth_manager,
            models_manager,
            installation_id,
        })
    }
}

#[async_trait::async_trait]
impl ToolRouterIntrospectionProvider for ToolRouterModelIntrospectionProvider {
    async fn run(
        &self,
        request: ToolRouterIntrospectionRequest,
    ) -> anyhow::Result<ToolRouterIntrospectionRawResponse> {
        let model_info = self
            .models_manager
            .get_model_info(&request.model, &self.config.to_models_manager_config())
            .await;
        let auth_manager = Some(auth_manager_for_config(&self.config, &self.auth_manager));
        let thread_id = ThreadId::new();
        let client = ModelClient::new(
            auth_manager,
            thread_id,
            self.installation_id.clone(),
            self.config.model_provider.clone(),
            SessionSource::Cli,
            self.config.model_verbosity,
            self.config
                .features
                .enabled(codex_features::Feature::EnableRequestCompression),
            self.config
                .features
                .enabled(codex_features::Feature::RuntimeMetrics),
            /*beta_features_header*/ None,
        );
        let session_telemetry = SessionTelemetry::new(
            thread_id,
            &request.model,
            &model_info.slug,
            /*account_id*/ None,
            /*account_email*/ None,
            /*auth_mode*/ None,
            "tool-router-tune".to_string(),
            /*log_user_prompts*/ false,
            "cli".to_string(),
            SessionSource::Cli,
        );
        let mut client_session = client.new_session();
        let prompt = Prompt {
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: request.prompt,
                }],
                phase: None,
            }],
            tools: Vec::new(),
            parallel_tool_calls: false,
            base_instructions: BaseInstructions {
                text: tool_router_introspection_instructions(),
            },
            personality: None,
            output_schema: Some(tool_router_introspection_output_schema()),
            output_schema_strict: true,
        };
        let mut stream = client_session
            .stream(
                &prompt,
                &model_info,
                &session_telemetry,
                self.config
                    .model_reasoning_effort
                    .or(model_info.default_reasoning_level),
                self.config.model_reasoning_summary.unwrap_or_default(),
                self.config.service_tier,
                None,
                &InferenceTraceContext::disabled(),
            )
            .await?;

        let mut output_text = String::new();
        let mut delta_text = String::new();
        let mut token_usage = ToolRouterTuneTokenUsage::default();
        while let Some(event) = stream.next().await {
            match event? {
                ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. }) => {
                    output_text = message_text(&content);
                }
                ResponseEvent::OutputTextDelta(delta) => delta_text.push_str(&delta),
                ResponseEvent::Completed {
                    token_usage: usage, ..
                } => {
                    if let Some(usage) = usage {
                        token_usage = ToolRouterTuneTokenUsage {
                            prompt_tokens: usage.input_tokens,
                            completion_tokens: usage
                                .output_tokens
                                .saturating_add(usage.reasoning_output_tokens),
                            total_tokens: usage.total_tokens,
                        };
                    }
                    break;
                }
                ResponseEvent::Created
                | ResponseEvent::OutputItemAdded(_)
                | ResponseEvent::OutputItemDone(_)
                | ResponseEvent::ServerModel(_)
                | ResponseEvent::ModelVerifications(_)
                | ResponseEvent::ServerReasoningIncluded(_)
                | ResponseEvent::ToolCallInputDelta { .. }
                | ResponseEvent::ReasoningSummaryDelta { .. }
                | ResponseEvent::ReasoningContentDelta { .. }
                | ResponseEvent::ReasoningSummaryPartAdded { .. }
                | ResponseEvent::RateLimits(_)
                | ResponseEvent::ModelsEtag(_) => {}
            }
        }
        if output_text.trim().is_empty() {
            output_text = delta_text;
        }
        Ok(ToolRouterIntrospectionRawResponse {
            output_text,
            token_usage,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneReport {
    pub window: String,
    pub apply: bool,
    pub introspection_model: Option<String>,
    pub introspection_tokens: ToolRouterTuneTokenUsage,
    pub schema_format_tokens: ToolRouterSchemaFormatTokens,
    pub optimizations: Vec<ToolRouterOptimizationReport>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterTuneTokenUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterSchemaFormatTokens {
    pub visible_router_schema_tokens: i64,
    pub hidden_tool_schema_tokens: i64,
    pub format_description_tokens: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRouterOptimizationReport {
    pub optimization_type: ToolRouterOptimizationType,
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
    pub guidance_version: i64,
    pub guidance_tokens_before: i64,
    pub guidance_tokens_after: i64,
    pub fallback_call_count: i64,
    pub invalid_route_errors: i64,
    pub affected_call_count: i64,
    pub per_call_estimated_savings_tokens: i64,
    pub gross_savings_tokens: i64,
    pub guidance_delta_cost_tokens: i64,
    pub allocated_introspection_tokens: i64,
    pub net_savings_tokens: i64,
    pub test_status: ToolRouterOptimizationTestStatus,
    pub persisted: bool,
    pub route_kind_breakdown: Vec<ToolRouterTuneCount>,
    pub selected_tool_breakdown: Vec<ToolRouterTuneCount>,
    pub fallback_tool_breakdown: Vec<ToolRouterTuneCount>,
    pub outcome_breakdown: Vec<ToolRouterTuneCount>,
    pub error_outcome_breakdown: Vec<ToolRouterTuneCount>,
    pub learned_rule_hits: i64,
    pub request_shape_clusters: Vec<ToolRouterRequestShapeCluster>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRouterOptimizationType {
    DropStaticGuidance,
    DynamicGuidance,
    FormatDescriptionRefresh,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolRouterOptimizationTestStatus {
    Passing,
    Failing,
}

pub async fn tune_tool_router(
    state_db: &StateRuntime,
    options: ToolRouterTuneOptions,
) -> anyhow::Result<ToolRouterTuneReport> {
    validate_tool_router_guidance_cap(options.max_guidance_tokens).map_err(anyhow::Error::msg)?;
    let window = parse_window(&options.window)?;
    let observations = state_db
        .tool_router_tune_observations(window, options.model_slug.as_deref())
        .await?;
    if options.introspection_model.is_some() && options.introspection_provider.is_none() {
        anyhow::bail!("--introspection-model requires a tune introspection provider");
    }
    let mut introspection_tokens = ToolRouterTuneTokenUsage::default();
    let mut optimizations = Vec::new();
    for observation in &observations {
        let introspection = introspection_for_observation(state_db, observation, &options).await?;
        introspection_tokens = add_token_usage(introspection_tokens, introspection.token_usage);
        optimizations.extend(optimizations_for_observation(
            observation,
            &options,
            introspection.guidance_result,
        ));
    }
    allocate_introspection_tokens(&mut optimizations, introspection_tokens.total_tokens);
    if options.apply {
        persist_passing_dynamic_guidance(state_db, &mut optimizations).await?;
    }

    Ok(ToolRouterTuneReport {
        window: options.window,
        apply: options.apply,
        introspection_model: options.introspection_model,
        introspection_tokens,
        schema_format_tokens: schema_format_tokens(&observations),
        optimizations,
    })
}

struct ToolRouterIntrospectionObservation {
    guidance_result: Option<Result<String, String>>,
    token_usage: ToolRouterTuneTokenUsage,
}

async fn introspection_for_observation(
    state_db: &StateRuntime,
    observation: &ToolRouterTuneObservation,
    options: &ToolRouterTuneOptions,
) -> anyhow::Result<ToolRouterIntrospectionObservation> {
    let Some(model) = options.introspection_model.as_ref() else {
        return Ok(ToolRouterIntrospectionObservation {
            guidance_result: None,
            token_usage: ToolRouterTuneTokenUsage::default(),
        });
    };
    if observation.fallback_call_count == 0 && observation.invalid_route_errors == 0 {
        return Ok(ToolRouterIntrospectionObservation {
            guidance_result: None,
            token_usage: ToolRouterTuneTokenUsage::default(),
        });
    }
    let Some(provider) = options.introspection_provider.as_ref() else {
        anyhow::bail!("--introspection-model requires a tune introspection provider");
    };

    let current_guidance = state_db
        .lookup_tool_router_guidance(&ToolRouterGuidanceKey {
            model_slug: observation.model_slug.clone(),
            model_provider: observation.model_provider.clone(),
            toolset_hash: observation.toolset_hash.clone(),
            router_schema_version: observation.router_schema_version,
        })
        .await?
        .map(|record| record.guidance_text);
    let request = ToolRouterIntrospectionRequest {
        model: model.clone(),
        prompt: tool_router_introspection_prompt(
            observation,
            current_guidance.as_deref(),
            options.max_guidance_tokens,
        ),
    };
    let raw = match provider.run(request).await {
        Ok(raw) => raw,
        Err(err) => {
            return Ok(ToolRouterIntrospectionObservation {
                guidance_result: Some(Err(format!("Introspection failed: {err:#}"))),
                token_usage: ToolRouterTuneTokenUsage::default(),
            });
        }
    };
    let guidance_result = parse_tool_router_introspection_output(
        raw.output_text.as_str(),
        options.max_guidance_tokens,
    )
    .map_err(|err| format!("Introspection guidance rejected: {err}"));
    Ok(ToolRouterIntrospectionObservation {
        guidance_result: Some(guidance_result),
        token_usage: raw.token_usage,
    })
}

fn tool_router_introspection_prompt(
    observation: &ToolRouterTuneObservation,
    current_guidance: Option<&str>,
    max_guidance_tokens: usize,
) -> String {
    let telemetry = json!({
        "modelSlug": observation.model_slug,
        "modelProvider": observation.model_provider,
        "toolsetHash": observation.toolset_hash,
        "routerSchemaVersion": observation.router_schema_version,
        "affectedCallCount": observation.affected_call_count,
        "fallbackCallCount": observation.fallback_call_count,
        "invalidRouteErrors": observation.invalid_route_errors,
        "routeKindCounts": observation.route_kind_breakdown,
        "topSelectedTools": observation.selected_tool_breakdown,
        "fallbackTools": observation.fallback_tool_breakdown,
        "outcomes": observation.outcome_breakdown,
        "learnedRuleHits": observation.learned_rule_hits,
        "errorOutcomes": observation.error_outcome_breakdown,
        "requestShapeClusters": observation.request_shape_clusters,
    });
    let telemetry = serde_json::to_string_pretty(&telemetry).unwrap_or_else(|_| "{}".to_string());
    let current_guidance = current_guidance.unwrap_or("(none)");
    let tool_catalog = compact_tool_catalog_for_prompt(observation);
    format!(
        "Generate dynamic guidance for `codex tool-router tune` using only the sanitized telemetry below.\n\nConstraints:\n- Return guidance that is specific to the repeated routing failures or fallbacks shown here.\n- Keep guidance short enough that the combined router guidance stays under {max_guidance_tokens} estimated tokens.\n- Do not include raw request text, commands, paths, URIs, IDs, or user-specific data.\n- Do not suggest persisting scripts or request-specific paths as learned rules.\n\nCurrent guidance:\n```text\n{current_guidance}\n```\n\nCompact tool catalog:\n{tool_catalog}\n\nSanitized telemetry:\n```json\n{telemetry}\n```"
    )
}

fn compact_tool_catalog_for_prompt(observation: &ToolRouterTuneObservation) -> String {
    if observation.selected_tool_breakdown.is_empty() {
        return "(no selected tools recorded)".to_string();
    }
    observation
        .selected_tool_breakdown
        .iter()
        .map(|tool| format!("- `{}`: observed {} routed calls", tool.name, tool.count))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct ToolRouterIntrospectionOutput {
    guidance: String,
    rationale: String,
    expected_improvement: String,
    risk: String,
    validation_notes: String,
}

fn parse_tool_router_introspection_output(
    output_text: &str,
    max_guidance_tokens: usize,
) -> Result<String, String> {
    let output: ToolRouterIntrospectionOutput = serde_json::from_str(output_text.trim())
        .map_err(|err| format!("model returned invalid JSON: {err}"))?;
    let guidance = output.guidance.trim();
    if guidance.is_empty() {
        return Err("guidance was empty".to_string());
    }
    let composed = compose_tool_router_guidance(Some(guidance), max_guidance_tokens);
    if !composed.dynamic_guidance_accepted {
        return Err(format!(
            "guidance exceeded max_guidance_tokens ({max_guidance_tokens})"
        ));
    }
    Ok(guidance.to_string())
}

fn tool_router_introspection_instructions() -> String {
    "You tune Codex's internal tool router. Return strict JSON only. Write one concise guidance string that can be reused for this model/toolset, based only on sanitized aggregate telemetry.".to_string()
}

fn tool_router_introspection_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["guidance", "rationale", "expected_improvement", "risk", "validation_notes"],
        "properties": {
            "guidance": {"type": "string"},
            "rationale": {"type": "string"},
            "expected_improvement": {"type": "string"},
            "risk": {"type": "string"},
            "validation_notes": {"type": "string"}
        }
    })
}

fn add_token_usage(
    left: ToolRouterTuneTokenUsage,
    right: ToolRouterTuneTokenUsage,
) -> ToolRouterTuneTokenUsage {
    ToolRouterTuneTokenUsage {
        prompt_tokens: left.prompt_tokens.saturating_add(right.prompt_tokens),
        completion_tokens: left
            .completion_tokens
            .saturating_add(right.completion_tokens),
        total_tokens: left.total_tokens.saturating_add(right.total_tokens),
    }
}

fn message_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn optimizations_for_observation(
    observation: &ToolRouterTuneObservation,
    options: &ToolRouterTuneOptions,
    introspection_guidance: Option<Result<String, String>>,
) -> Vec<ToolRouterOptimizationReport> {
    let mut optimizations = Vec::new();
    let static_tokens = i64::try_from(tool_router_static_guidelines_tokens()).unwrap_or(i64::MAX);
    optimizations.push(report(OptimizationDraft {
        optimization_type: ToolRouterOptimizationType::DropStaticGuidance,
        observation,
        guidance_tokens_before: observation.guidance_tokens.saturating_add(static_tokens),
        guidance_tokens_after: observation.guidance_tokens,
        affected_call_count: observation.affected_call_count,
        per_call_estimated_savings_tokens: static_tokens,
        gross_savings_tokens: None,
        test_status: ToolRouterOptimizationTestStatus::Passing,
        message: "Static Tool Guidelines are removed from bundled base instructions only when tool_router is active.".to_string(),
    }));

    if observation.fallback_call_count > 0 || observation.invalid_route_errors > 0 {
        let mut forced_status = None;
        let dynamic_guidance = match introspection_guidance {
            Some(Ok(guidance)) => guidance,
            Some(Err(message)) => {
                forced_status = Some(ToolRouterOptimizationTestStatus::Failing);
                message
            }
            None => dynamic_guidance_for_observation(observation),
        };
        let composed = compose_tool_router_guidance(
            Some(dynamic_guidance.as_str()),
            options.max_guidance_tokens,
        );
        let dynamic_tokens = i64::try_from(composed.tokens).unwrap_or(i64::MAX);
        let test_status = forced_status.unwrap_or(if composed.dynamic_guidance_accepted {
            ToolRouterOptimizationTestStatus::Passing
        } else {
            ToolRouterOptimizationTestStatus::Failing
        });
        let fallback_token_total = observation
            .fallback_prompt_tokens
            .saturating_add(observation.fallback_completion_tokens);
        let affected_calls = observation
            .fallback_call_count
            .saturating_add(observation.invalid_route_errors);
        let per_call_savings = if observation.fallback_call_count > 0 {
            fallback_token_total / observation.fallback_call_count
        } else {
            0
        };
        optimizations.push(report(OptimizationDraft {
            optimization_type: ToolRouterOptimizationType::DynamicGuidance,
            observation,
            guidance_tokens_before: observation.guidance_tokens,
            guidance_tokens_after: dynamic_tokens,
            affected_call_count: affected_calls,
            per_call_estimated_savings_tokens: per_call_savings,
            gross_savings_tokens: Some(fallback_token_total),
            test_status,
            message: dynamic_guidance,
        }));
    }

    optimizations.push(report(OptimizationDraft {
        optimization_type: ToolRouterOptimizationType::FormatDescriptionRefresh,
        observation,
        guidance_tokens_before: observation.guidance_tokens,
        guidance_tokens_after: observation.guidance_tokens,
        affected_call_count: observation.affected_call_count,
        per_call_estimated_savings_tokens: 0,
        gross_savings_tokens: None,
        test_status: ToolRouterOptimizationTestStatus::Passing,
        message: "Router format description is regenerated from the current tool catalog and is not counted against the guidance cap.".to_string(),
    }));
    optimizations
}

struct OptimizationDraft<'a> {
    optimization_type: ToolRouterOptimizationType,
    observation: &'a ToolRouterTuneObservation,
    guidance_tokens_before: i64,
    guidance_tokens_after: i64,
    affected_call_count: i64,
    per_call_estimated_savings_tokens: i64,
    gross_savings_tokens: Option<i64>,
    test_status: ToolRouterOptimizationTestStatus,
    message: String,
}

fn report(draft: OptimizationDraft<'_>) -> ToolRouterOptimizationReport {
    let OptimizationDraft {
        optimization_type,
        observation,
        guidance_tokens_before,
        guidance_tokens_after,
        affected_call_count,
        per_call_estimated_savings_tokens,
        gross_savings_tokens,
        test_status,
        message,
    } = draft;
    let gross_savings_tokens = gross_savings_tokens.unwrap_or_else(|| {
        affected_call_count
            .max(0)
            .saturating_mul(per_call_estimated_savings_tokens.max(0))
    });
    let guidance_delta_cost_tokens = guidance_tokens_after
        .saturating_sub(guidance_tokens_before)
        .max(0)
        .saturating_mul(observation.affected_call_count.max(0));
    let mut report = ToolRouterOptimizationReport {
        optimization_type,
        model_slug: observation.model_slug.clone(),
        model_provider: observation.model_provider.clone(),
        toolset_hash: observation.toolset_hash.clone(),
        router_schema_version: observation.router_schema_version,
        guidance_version: match optimization_type {
            ToolRouterOptimizationType::DynamicGuidance => DYNAMIC_GUIDANCE_VERSION,
            ToolRouterOptimizationType::DropStaticGuidance
            | ToolRouterOptimizationType::FormatDescriptionRefresh => {
                TOOL_ROUTER_DEFAULT_GUIDANCE_VERSION
            }
        },
        guidance_tokens_before,
        guidance_tokens_after,
        fallback_call_count: observation.fallback_call_count,
        invalid_route_errors: observation.invalid_route_errors,
        affected_call_count,
        per_call_estimated_savings_tokens,
        gross_savings_tokens,
        guidance_delta_cost_tokens,
        allocated_introspection_tokens: 0,
        net_savings_tokens: gross_savings_tokens.saturating_sub(guidance_delta_cost_tokens),
        test_status,
        persisted: false,
        route_kind_breakdown: observation.route_kind_breakdown.clone(),
        selected_tool_breakdown: observation.selected_tool_breakdown.clone(),
        fallback_tool_breakdown: observation.fallback_tool_breakdown.clone(),
        outcome_breakdown: observation.outcome_breakdown.clone(),
        error_outcome_breakdown: observation.error_outcome_breakdown.clone(),
        learned_rule_hits: observation.learned_rule_hits,
        request_shape_clusters: observation.request_shape_clusters.clone(),
        message,
    };
    recompute_net_savings(&mut report);
    report
}

fn recompute_net_savings(optimization: &mut ToolRouterOptimizationReport) {
    optimization.net_savings_tokens = optimization
        .gross_savings_tokens
        .saturating_sub(optimization.guidance_delta_cost_tokens)
        .saturating_sub(optimization.allocated_introspection_tokens);
}

fn dynamic_guidance_for_observation(observation: &ToolRouterTuneObservation) -> String {
    let mut guidance = vec![
        "For this model/toolset, avoid fallback routing by sending deterministic `tool_router` inputs."
            .to_string(),
    ];
    if observation.invalid_route_errors > 0 {
        guidance.push("Invalid routes were observed; include required `request`, `where`, `targets`, and `action` fields, and use exact `action.tool` when a routed tool is known.".to_string());
    }
    if !observation.fallback_tool_breakdown.is_empty() {
        let tools = observation
            .fallback_tool_breakdown
            .iter()
            .take(3)
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        guidance.push(format!(
            "Fallbacks most often selected {tools}; prefer those exact tool names in `action.tool` when they are the intended destination."
        ));
    }
    if observation
        .route_kind_breakdown
        .iter()
        .any(|entry| matches!(entry.name.as_str(), "model_router_script" | "spark_script"))
    {
        guidance.push("Script fallbacks were observed; for shell work set `where.kind` to `shell` and provide concrete `action.cmd`, `action.command`, or `action.commands`.".to_string());
    }
    guidance.push("For filesystem, search, image, MCP, repo-ci, and process work, include the relevant path/query/namespace/session targets plus the smallest sufficient `action.kind`.".to_string());
    guidance.join(" ")
}

fn allocate_introspection_tokens(
    optimizations: &mut [ToolRouterOptimizationReport],
    introspection_total_tokens: i64,
) {
    if introspection_total_tokens <= 0 {
        return;
    }
    let gross_total = optimizations
        .iter()
        .map(|optimization| optimization.gross_savings_tokens.max(0))
        .sum::<i64>();
    if gross_total <= 0 {
        return;
    }
    let mut allocated = 0;
    let last_index = optimizations.len().saturating_sub(1);
    for (index, optimization) in optimizations.iter_mut().enumerate() {
        let share = if index == last_index {
            introspection_total_tokens.saturating_sub(allocated)
        } else {
            optimization
                .gross_savings_tokens
                .saturating_mul(introspection_total_tokens)
                / gross_total
        };
        allocated = allocated.saturating_add(share);
        optimization.allocated_introspection_tokens = share;
        recompute_net_savings(optimization);
    }
}

async fn persist_passing_dynamic_guidance(
    state_db: &StateRuntime,
    optimizations: &mut [ToolRouterOptimizationReport],
) -> anyhow::Result<()> {
    for optimization in optimizations.iter_mut() {
        if optimization.optimization_type != ToolRouterOptimizationType::DynamicGuidance
            || optimization.test_status != ToolRouterOptimizationTestStatus::Passing
        {
            continue;
        }
        let record = ToolRouterGuidanceRecord {
            key: ToolRouterGuidanceKey {
                model_slug: optimization.model_slug.clone(),
                model_provider: optimization.model_provider.clone(),
                toolset_hash: optimization.toolset_hash.clone(),
                router_schema_version: optimization.router_schema_version,
            },
            guidance_version: optimization.guidance_version,
            guidance_text: optimization.message.clone(),
            guidance_tokens: optimization.guidance_tokens_after,
            source: "codex tool-router tune".to_string(),
        };
        state_db.upsert_tool_router_guidance(record).await?;
        optimization.persisted = true;
    }
    Ok(())
}

fn schema_format_tokens(
    observations: &[ToolRouterTuneObservation],
) -> ToolRouterSchemaFormatTokens {
    let visible_router_schema_tokens = observations
        .iter()
        .map(|observation| observation.visible_router_schema_tokens)
        .max()
        .unwrap_or_default();
    let hidden_tool_schema_tokens = observations
        .iter()
        .map(|observation| observation.hidden_tool_schema_tokens)
        .max()
        .unwrap_or_default();
    ToolRouterSchemaFormatTokens {
        visible_router_schema_tokens,
        hidden_tool_schema_tokens,
        format_description_tokens: observations
            .iter()
            .map(|observation| observation.format_description_tokens)
            .max()
            .unwrap_or_default(),
    }
}

fn parse_window(window: &str) -> anyhow::Result<ToolRouterDiagnosticsWindow> {
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return Ok(ToolRouterDiagnosticsWindow::AllTime);
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let value = number
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("window must be a duration like 7d, 24h, 30m, or all"))?;
    let multiplier = match unit {
        "d" => 24 * 60 * 60 * 1000,
        "h" => 60 * 60 * 1000,
        "m" => 60 * 1000,
        _ => {
            return Err(anyhow::anyhow!(
                "window must be a duration like 7d, 24h, 30m, or all"
            ));
        }
    };
    let window_ms = value.max(0).saturating_mul(multiplier);
    Ok(ToolRouterDiagnosticsWindow::SinceCreatedAtMs(
        Utc::now().timestamp_millis().saturating_sub(window_ms),
    ))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tempfile::TempDir;

    use codex_state::ToolRouterLedgerEntry;
    use codex_state::ToolRouterTuneCount;

    use super::*;

    #[tokio::test]
    async fn dry_run_reports_optimizations_without_persisting() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 25, 5))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ false, 600))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && !optimization.persisted
        }));
        let dynamic = report
            .optimizations
            .iter()
            .find(|optimization| {
                optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
            })
            .expect("dynamic guidance optimization");
        assert!(
            dynamic
                .message
                .contains("Fallbacks most often selected list_dir")
        );
        assert_eq!(
            dynamic.fallback_tool_breakdown,
            vec![ToolRouterTuneCount {
                name: "list_dir".to_string(),
                count: 1,
            }]
        );
        assert_eq!(dynamic.gross_savings_tokens, 30);
        assert!(dynamic.guidance_delta_cost_tokens > 0);
        let key = guidance_key();
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&key)
                .await
                .expect("lookup guidance"),
            None
        );
    }

    #[tokio::test]
    async fn apply_persists_only_passing_dynamic_guidance() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 300, 60))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ true, 600))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && optimization.persisted
        }));
        let record = runtime
            .lookup_tool_router_guidance(&guidance_key())
            .await
            .expect("lookup guidance")
            .expect("guidance record");
        assert_eq!(record.guidance_version, DYNAMIC_GUIDANCE_VERSION);
    }

    #[tokio::test]
    async fn over_cap_dynamic_guidance_is_not_persisted() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");

        let report = tune_tool_router(&runtime, options(/*apply*/ true, 1))
            .await
            .expect("tune report");

        assert!(report.optimizations.iter().any(|optimization| {
            optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
                && optimization.test_status == ToolRouterOptimizationTestStatus::Failing
                && !optimization.persisted
        }));
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&guidance_key())
                .await
                .expect("lookup guidance"),
            None
        );
    }

    #[tokio::test]
    async fn dynamic_guidance_savings_count_only_fallback_tokens() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record fallback");
        runtime
            .record_tool_router_ledger_entry(ledger_entry("error", 99, 99))
            .await
            .expect("record error");

        let report = tune_tool_router(&runtime, options(/*apply*/ false, 600))
            .await
            .expect("tune report");
        let dynamic = dynamic_optimization(&report);

        assert_eq!(dynamic.affected_call_count, 2);
        assert_eq!(dynamic.fallback_call_count, 1);
        assert_eq!(dynamic.invalid_route_errors, 1);
        assert_eq!(dynamic.per_call_estimated_savings_tokens, 36);
        assert_eq!(dynamic.gross_savings_tokens, 36);
    }

    #[tokio::test]
    async fn dynamic_guidance_error_only_has_zero_savings() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("error", 99, 99))
            .await
            .expect("record error");

        let report = tune_tool_router(&runtime, options(/*apply*/ false, 600))
            .await
            .expect("tune report");
        let dynamic = dynamic_optimization(&report);

        assert_eq!(dynamic.affected_call_count, 1);
        assert_eq!(dynamic.per_call_estimated_savings_tokens, 0);
        assert_eq!(dynamic.gross_savings_tokens, 0);
    }

    #[tokio::test]
    async fn introspection_guidance_persists_when_valid_and_applied() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");
        let guidance = "For this shell-heavy toolset, route known shell execution directly with `action.tool` and `action.cmd`.";

        let report = tune_tool_router(
            &runtime,
            introspection_options(
                /*apply*/ true,
                600,
                valid_introspection_output(guidance),
            ),
        )
        .await
        .expect("tune report");

        let dynamic = dynamic_optimization(&report);
        assert_eq!(dynamic.message, guidance);
        assert!(dynamic.persisted);
        assert_eq!(report.introspection_tokens.total_tokens, 7);
        let record = runtime
            .lookup_tool_router_guidance(&guidance_key())
            .await
            .expect("lookup guidance")
            .expect("guidance record");
        assert_eq!(record.guidance_text, guidance);
    }

    #[tokio::test]
    async fn introspection_over_cap_guidance_is_not_persisted() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");

        let report = tune_tool_router(
            &runtime,
            introspection_options(
                /*apply*/ true,
                20,
                valid_introspection_output(&"route direct tools ".repeat(200)),
            ),
        )
        .await
        .expect("tune report");

        let dynamic = dynamic_optimization(&report);
        assert_eq!(
            dynamic.test_status,
            ToolRouterOptimizationTestStatus::Failing
        );
        assert!(!dynamic.persisted);
        assert!(dynamic.message.contains("exceeded max_guidance_tokens"));
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&guidance_key())
                .await
                .expect("lookup guidance"),
            None
        );
    }

    #[tokio::test]
    async fn malformed_introspection_output_is_not_persisted() {
        let (_codex_home, runtime) = state_runtime().await;
        runtime
            .record_tool_router_ledger_entry(ledger_entry("model_router", 30, 6))
            .await
            .expect("record ledger");

        let report = tune_tool_router(
            &runtime,
            introspection_options(/*apply*/ true, 600, "not json".to_string()),
        )
        .await
        .expect("tune report");

        let dynamic = dynamic_optimization(&report);
        assert_eq!(
            dynamic.test_status,
            ToolRouterOptimizationTestStatus::Failing
        );
        assert!(!dynamic.persisted);
        assert!(dynamic.message.contains("invalid JSON"));
        assert_eq!(
            runtime
                .lookup_tool_router_guidance(&guidance_key())
                .await
                .expect("lookup guidance"),
            None
        );
    }

    #[test]
    fn introspection_output_schema_is_strict() {
        let schema = tool_router_introspection_output_schema();

        assert_eq!(schema.get("additionalProperties"), Some(&json!(false)));
        assert_eq!(
            schema.get("required"),
            Some(&json!([
                "guidance",
                "rationale",
                "expected_improvement",
                "risk",
                "validation_notes"
            ]))
        );
    }

    #[test]
    fn introspection_prompt_uses_sanitized_telemetry() {
        let observation = ToolRouterTuneObservation {
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
            affected_call_count: 2,
            fallback_call_count: 1,
            fallback_prompt_tokens: 30,
            fallback_completion_tokens: 6,
            invalid_route_errors: 1,
            guidance_tokens: 10,
            format_description_tokens: 40,
            visible_router_schema_tokens: 12,
            hidden_tool_schema_tokens: 120,
            route_kind_breakdown: vec![ToolRouterTuneCount {
                name: "model_router".to_string(),
                count: 1,
            }],
            selected_tool_breakdown: vec![ToolRouterTuneCount {
                name: "exec_command".to_string(),
                count: 1,
            }],
            fallback_tool_breakdown: Vec::new(),
            outcome_breakdown: Vec::new(),
            error_outcome_breakdown: Vec::new(),
            learned_rule_hits: 0,
            request_shape_clusters: Vec::new(),
        };

        let prompt = tool_router_introspection_prompt(&observation, Some("current"), 600);

        assert!(prompt.contains("Sanitized telemetry"));
        assert!(prompt.contains("exec_command"));
        assert!(prompt.contains("Do not include raw request text"));
        assert!(!prompt.contains("Original user intent"));
    }

    fn options(apply: bool, max_guidance_tokens: usize) -> ToolRouterTuneOptions {
        ToolRouterTuneOptions {
            window: "all".to_string(),
            model_slug: Some("gpt-test".to_string()),
            max_guidance_tokens,
            introspection_model: None,
            introspection_provider: None,
            apply,
        }
    }

    fn introspection_options(
        apply: bool,
        max_guidance_tokens: usize,
        output_text: String,
    ) -> ToolRouterTuneOptions {
        ToolRouterTuneOptions {
            window: "all".to_string(),
            model_slug: Some("gpt-test".to_string()),
            max_guidance_tokens,
            introspection_model: Some("gpt-introspect".to_string()),
            introspection_provider: Some(Arc::new(StaticIntrospectionProvider { output_text })),
            apply,
        }
    }

    fn dynamic_optimization(report: &ToolRouterTuneReport) -> &ToolRouterOptimizationReport {
        report
            .optimizations
            .iter()
            .find(|optimization| {
                optimization.optimization_type == ToolRouterOptimizationType::DynamicGuidance
            })
            .expect("dynamic optimization")
    }

    fn valid_introspection_output(guidance: &str) -> String {
        json!({
            "guidance": guidance,
            "rationale": "observed repeated fallback shape",
            "expected_improvement": "fewer fallback calls",
            "risk": "low",
            "validation_notes": "under cap"
        })
        .to_string()
    }

    struct StaticIntrospectionProvider {
        output_text: String,
    }

    #[async_trait::async_trait]
    impl ToolRouterIntrospectionProvider for StaticIntrospectionProvider {
        async fn run(
            &self,
            _request: ToolRouterIntrospectionRequest,
        ) -> anyhow::Result<ToolRouterIntrospectionRawResponse> {
            Ok(ToolRouterIntrospectionRawResponse {
                output_text: self.output_text.clone(),
                token_usage: ToolRouterTuneTokenUsage {
                    prompt_tokens: 3,
                    completion_tokens: 4,
                    total_tokens: 7,
                },
            })
        }
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }

    fn guidance_key() -> ToolRouterGuidanceKey {
        ToolRouterGuidanceKey {
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
        }
    }

    fn ledger_entry(
        route_kind: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
    ) -> ToolRouterLedgerEntry {
        ToolRouterLedgerEntry {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            call_id: "call".to_string(),
            model_slug: "gpt-test".to_string(),
            model_provider: "openai".to_string(),
            toolset_hash: "abc123".to_string(),
            router_schema_version: 1,
            guidance_version: 1,
            guidance_tokens: 10,
            format_description_tokens: 40,
            route_kind: route_kind.to_string(),
            selected_tools: vec!["list_dir".to_string()],
            visible_router_schema_tokens: 12,
            hidden_tool_schema_tokens: 120,
            spark_prompt_tokens: prompt_tokens,
            spark_completion_tokens: completion_tokens,
            fanout_call_count: 1,
            returned_output_tokens: 0,
            original_output_tokens: 0,
            truncated_output_tokens: 0,
            outcome: Some("ok".to_string()),
            request_shape_json: None,
        }
    }
}
