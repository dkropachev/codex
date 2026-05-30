use std::sync::Arc;
use std::time::Instant;

use codex_model_router::RouterRequestKind;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use codex_state::ModelRouterLedgerEntry;
use codex_state::ModelRouterShadowEvaluationRecord;
use codex_state::StateRuntime;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tracing::debug;

use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::model_router::ModelRouterShadowPlan;
use crate::model_router::apply_candidate;
use crate::model_router::available_router_models;
use crate::model_router::model_client_for_config;
use crate::model_router::model_router_shadow_plan;
use crate::model_router::token_price_from_candidate;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

const SHADOW_BASE_INSTRUCTIONS_SUFFIX: &str = "\n\nShadow evaluation: answer the current user turn directly. Do not mention shadow evaluation. Do not call tools or request follow-up tool access; use any tool results already present in the transcript.";
const JUDGE_BASE_INSTRUCTIONS: &str = "Judge whether a shadow model answer is at least as useful and correct as the production answer. Return only JSON matching the schema.";

pub(crate) fn spawn_model_router_shadow_evaluation(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    prompt_input: Vec<ResponseItem>,
    production_output: Option<String>,
) {
    let Some(production_output) = production_output.filter(|output| !output.trim().is_empty())
    else {
        return;
    };
    if !turn_context
        .config
        .model_router
        .as_ref()
        .is_some_and(|model_router| model_router.enabled)
    {
        return;
    }
    let Some(task_key) = turn_context.tool_router_task_key.clone() else {
        return;
    };
    let prompt_bytes = prompt_input_bytes(&prompt_input);
    let runtime_handle = sess.services.runtime_handle.clone();
    runtime_handle.spawn(async move {
        if let Err(err) = run_model_router_shadow_evaluation(
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            task_key,
            prompt_bytes,
            prompt_input,
            production_output,
        )
        .await
        {
            debug!(turn_id = %turn_context.sub_id, error = %err, "model router shadow evaluation failed");
        }
    });
}

async fn run_model_router_shadow_evaluation(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    task_key: String,
    prompt_bytes: usize,
    prompt_input: Vec<ResponseItem>,
    production_output: String,
) -> anyhow::Result<()> {
    let Some(state_db) = sess.services.state_db.clone() else {
        return Ok(());
    };
    let available_models = available_router_models(
        &turn_context.config,
        &sess.services.models_manager,
        &sess.services.model_router_discovery_cache,
    )
    .await;
    let Some(plan) = model_router_shadow_plan(
        &turn_context.config,
        &task_key,
        prompt_bytes,
        &available_models,
        Some(state_db.as_ref()),
        &[],
    )
    .await
    else {
        return Ok(());
    };

    let mut candidate_config = turn_context.config.as_ref().clone();
    apply_candidate(&mut candidate_config, &plan.candidate).map_err(anyhow::Error::msg)?;
    candidate_config.model_router = None;
    candidate_config.model_router_accounting = None;
    let model_client = model_client_for_config(
        &candidate_config,
        &sess.services.model_client,
        &sess.services.auth_manager,
    );
    let mut base_instructions = sess.get_base_instructions().await;
    base_instructions
        .text
        .push_str(SHADOW_BASE_INSTRUCTIONS_SUFFIX);
    let shadow_prompt = Prompt {
        input: prompt_input,
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions,
        personality: turn_context.personality,
        output_schema: turn_context.final_output_json_schema.clone(),
        output_schema_strict: turn_context.final_output_json_schema.is_some(),
    };
    let shadow = run_shadow_turn(
        &candidate_config,
        &model_client,
        &sess.services.models_manager,
        &turn_context,
        shadow_prompt,
    )
    .await?;
    record_shadow_ledger_entry(
        state_db.as_ref(),
        &plan,
        &candidate_config,
        shadow.token_usage.clone(),
        &format!("live_shadow.{}", plan.phase),
    )
    .await;
    if shadow.text.trim().is_empty() {
        return Ok(());
    }

    let judge = run_judge_turn(
        sess.as_ref(),
        &turn_context,
        production_output,
        shadow.text.clone(),
    )
    .await?;
    record_judge_ledger_entry(
        state_db.as_ref(),
        &task_key,
        &turn_context.config,
        judge.token_usage.clone(),
    )
    .await;
    let parsed = parse_judge_output(&judge.text)?;
    let shadow_cost_usd_micros = candidate_cost_usd_micros(&shadow.token_usage, &plan);
    state_db
        .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
            id: None,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            task_key,
            phase: plan.phase,
            candidate_identity: plan.candidate_identity,
            base_candidate_identity: plan.base_candidate_identity,
            success: parsed.pass,
            score: Some(parsed.score.clamp(0.0, 1.0)),
            confidence: parsed.confidence.clamp(0.0, 1.0),
            cost_usd_micros: shadow_cost_usd_micros,
            total_tokens: shadow.token_usage.total_tokens,
            outcome: Some("live_shadow_judged".to_string()),
            metadata_json: Some(
                json!({
                    "turnId": turn_context.sub_id.as_str(),
                    "threadId": sess.conversation_id.to_string(),
                    "shadowDurationMs": shadow.duration_ms,
                    "judgeDurationMs": judge.duration_ms,
                })
                .to_string(),
            ),
        })
        .await?;
    Ok(())
}

async fn run_judge_turn(
    sess: &Session,
    turn_context: &TurnContext,
    production_output: String,
    candidate_output: String,
) -> anyhow::Result<ShadowTurnOutput> {
    let mut judge_config = turn_context.config.as_ref().clone();
    if let Some(model_router) = judge_config.model_router.as_mut() {
        model_router.enabled = false;
    }
    judge_config.model_router_accounting = None;
    let prompt_text = format!(
        "Production answer:\n{production_output}\n\nShadow answer:\n{candidate_output}\n\nReturn JSON with pass, score, and confidence."
    );
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: prompt_text }],
            phase: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: JUDGE_BASE_INSTRUCTIONS.to_string(),
        },
        personality: None,
        output_schema: Some(judge_output_schema()),
        output_schema_strict: true,
    };
    let judge_model_client = model_client_for_config(
        &judge_config,
        &sess.services.model_client,
        &sess.services.auth_manager,
    );
    run_shadow_turn(
        &judge_config,
        &judge_model_client,
        &sess.services.models_manager,
        turn_context,
        prompt,
    )
    .await
}

async fn run_shadow_turn(
    config: &Config,
    model_client: &ModelClient,
    models_manager: &SharedModelsManager,
    turn_context: &TurnContext,
    prompt: Prompt,
) -> anyhow::Result<ShadowTurnOutput> {
    let model = config
        .model
        .clone()
        .ok_or_else(|| anyhow::anyhow!("model router shadow requires a configured model"))?;
    let model_info = models_manager
        .get_model_info(model.as_str(), &config.to_models_manager_config())
        .await;
    let mut client_session = model_client.new_session();
    let start = Instant::now();
    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            &turn_context.session_telemetry,
            config.model_reasoning_effort,
            config
                .model_reasoning_summary
                .unwrap_or(ReasoningSummaryConfig::Auto),
            config
                .service_tier
                .map(|service_tier| service_tier.request_value().to_string()),
            /*turn_metadata_header*/ None,
            &codex_rollout_trace::InferenceTraceContext::disabled(),
        )
        .await?;

    let mut output_text = String::new();
    let mut delta_text = String::new();
    let mut token_usage = TokenUsage::default();
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
                    token_usage = usage;
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
    Ok(ShadowTurnOutput {
        text: output_text,
        token_usage,
        duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

async fn record_shadow_ledger_entry(
    state_db: &StateRuntime,
    plan: &ModelRouterShadowPlan,
    config: &Config,
    token_usage: TokenUsage,
    outcome: &str,
) {
    let (actual_cost_usd_micros, price_confidence) = candidate_cost_estimate(&token_usage, plan);
    let _ = state_db
        .record_model_router_ledger_entry(ModelRouterLedgerEntry {
            task_key: plan.task_key.clone(),
            request_kind: RouterRequestKind::Shadow,
            model_provider: Some(config.model_provider_id.clone()),
            model: config.model.clone(),
            account_id: plan
                .candidate
                .account
                .clone()
                .or_else(|| plan.candidate.account_pool.clone()),
            token_usage,
            actual_cost_usd_micros,
            counterfactual_cost_usd_micros: 0,
            price_confidence,
            outcome: Some(outcome.to_string()),
        })
        .await;
}

async fn record_judge_ledger_entry(
    state_db: &StateRuntime,
    task_key: &str,
    config: &Config,
    token_usage: TokenUsage,
) {
    let _ = state_db
        .record_model_router_ledger_entry(ModelRouterLedgerEntry {
            task_key: task_key.to_string(),
            request_kind: RouterRequestKind::Judge,
            model_provider: Some(config.model_provider_id.clone()),
            model: config.model.clone(),
            account_id: None,
            token_usage,
            actual_cost_usd_micros: 0,
            counterfactual_cost_usd_micros: 0,
            price_confidence: 0.0,
            outcome: Some("live_shadow_judge".to_string()),
        })
        .await;
}

fn candidate_cost_estimate(usage: &TokenUsage, plan: &ModelRouterShadowPlan) -> (i64, f64) {
    token_price_from_candidate(&plan.candidate)
        .map(|price| {
            (
                codex_model_router::estimate_token_cost(usage, &price, /*confidence*/ 1.0)
                    .usd_micros,
                1.0,
            )
        })
        .unwrap_or((0, 0.0))
}

fn candidate_cost_usd_micros(usage: &TokenUsage, plan: &ModelRouterShadowPlan) -> i64 {
    candidate_cost_estimate(usage, plan).0
}

fn prompt_input_bytes(input: &[ResponseItem]) -> usize {
    input
        .iter()
        .filter_map(|item| serde_json::to_vec(item).ok())
        .map(|item| item.len())
        .sum::<usize>()
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
        .join("\n")
}

fn parse_judge_output(output_text: &str) -> anyhow::Result<JudgeOutput> {
    serde_json::from_str(output_text.trim()).map_err(anyhow::Error::from)
}

fn judge_output_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "pass": { "type": "boolean" },
            "score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
            "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
        },
        "required": ["pass", "score", "confidence"]
    })
}

#[derive(Debug)]
struct ShadowTurnOutput {
    text: String,
    token_usage: TokenUsage,
    duration_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JudgeOutput {
    pass: bool,
    score: f64,
    confidence: f64,
}
