use std::time::Instant;

use codex_features::Feature;
use codex_model_router::RouterRequestKind;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsage;
use codex_state::ModelRouterLedgerEntry;
use codex_state::ModelRouterLifecyclePromotionRecord;
use codex_state::StateRuntime;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::model_router::apply_candidate;
use crate::model_router::auth_manager_for_config;
use crate::model_router::model_router_candidate_identity_key;
use crate::model_router::token_price_from_candidate;

use super::MIN_APPLY_CONFIDENCE;
use super::ModelRouterMetricName;
use super::ModelRouterMetricValue;
use super::ModelRouterTuneBudgetUsed;
use super::ModelRouterTuneRuntime;
use super::ReplayCase;

const REPLAY_BASE_INSTRUCTIONS: &str = "Replay this historical Codex turn in isolation. Respond to the user request directly. Do not call tools, ask for approval, write memory, or assume live workspace mutation.";
const JUDGE_BASE_INSTRUCTIONS: &str = "Judge whether a replayed Codex answer is at least as useful and correct as the historical production answer. Return only JSON matching the schema.";

pub(super) struct CandidateTuneEvaluation {
    pub(super) metrics: Vec<(ModelRouterMetricName, ModelRouterMetricValue)>,
    pub(super) evaluated_count: i64,
    pub(super) skipped_count: i64,
    pub(super) confidence: f64,
    pub(super) passing: bool,
    pub(super) budget_used: ModelRouterTuneBudgetUsed,
    pub(super) shadow_evaluations: Vec<CandidateShadowEvaluation>,
}

pub(super) struct CandidateShadowEvaluation {
    pub(super) task_key: String,
    pub(super) candidate_identity: String,
    pub(super) base_candidate_identity: String,
    pub(super) success: bool,
    pub(super) score: f64,
    pub(super) confidence: f64,
    pub(super) cost_usd_micros: i64,
    pub(super) total_tokens: i64,
}

pub(super) async fn evaluate_candidate(
    state_db: &StateRuntime,
    config: &Config,
    candidate: &codex_config::config_toml::ModelRouterCandidateToml,
    cases: &[ReplayCase],
    promotions: &[ModelRouterLifecyclePromotionRecord],
    runtime: &ModelRouterTuneRuntime,
) -> anyhow::Result<CandidateTuneEvaluation> {
    let mut scores = Vec::new();
    let mut confidences = Vec::new();
    let mut pass_count = 0_i64;
    let mut latencies = Vec::new();
    let mut costs = Vec::new();
    let mut shadow_evaluations = Vec::new();
    let mut budget_used = ModelRouterTuneBudgetUsed {
        cost_used_usd_micros: 0,
        tokens_used: 0,
    };
    let candidate_identity = model_router_candidate_identity_key(candidate);

    for case in cases {
        let replay = replay_candidate_case(config, candidate, case, runtime).await?;
        add_turn_budget(&mut budget_used, &replay.token_usage, Some(candidate));
        let (replay_cost_usd_micros, replay_price_confidence) =
            candidate_cost_estimate(&replay.token_usage, candidate);
        let shadow_phase =
            shadow_phase_for_case(promotions, case.task_key.as_str(), &candidate_identity);
        let replay_outcome = format!("tune_replay.{shadow_phase}");
        record_model_router_tune_ledger_entry(
            state_db,
            case.task_key.as_str(),
            RouterRequestKind::Shadow,
            candidate
                .model_provider
                .as_deref()
                .unwrap_or(config.model_provider_id.as_str()),
            candidate.model.clone().or_else(|| config.model.clone()),
            candidate
                .account
                .clone()
                .or_else(|| candidate.account_pool.clone()),
            replay.token_usage.clone(),
            replay_cost_usd_micros,
            replay_price_confidence,
            replay_outcome.as_str(),
        )
        .await;
        if replay.text.trim().is_empty() {
            continue;
        }
        latencies.push(replay.duration_ms);
        if let Some(price) = token_price_from_candidate(candidate) {
            costs.push(
                codex_model_router::estimate_token_cost(
                    &replay.token_usage,
                    &price,
                    /*confidence*/ 1.0,
                )
                .usd_micros,
            );
        }

        let judge = judge_candidate_case(config, case, &replay.text, runtime).await?;
        add_turn_budget(&mut budget_used, &judge.token_usage, None);
        record_model_router_tune_ledger_entry(
            state_db,
            case.task_key.as_str(),
            RouterRequestKind::Judge,
            config.model_provider_id.as_str(),
            config.model.clone(),
            /*account_id*/ None,
            judge.token_usage.clone(),
            /*actual_cost_usd_micros*/ 0,
            /*price_confidence*/ 0.0,
            "tune_judge",
        )
        .await;
        shadow_evaluations.push(CandidateShadowEvaluation {
            task_key: case.task_key.clone(),
            candidate_identity: candidate_identity.clone(),
            base_candidate_identity: historical_production_identity_key(case),
            success: judge.pass,
            score: judge.score,
            confidence: judge.confidence,
            cost_usd_micros: replay_cost_usd_micros,
            total_tokens: replay.token_usage.total_tokens,
        });
        scores.push(judge.score);
        confidences.push(judge.confidence);
        if judge.pass {
            pass_count += 1;
        }
    }

    let evaluated_count = i64::try_from(scores.len()).unwrap_or(i64::MAX);
    let skipped_count = i64::try_from(cases.len().saturating_sub(scores.len())).unwrap_or(0);
    let success_rate = if evaluated_count > 0 {
        pass_count as f64 / evaluated_count as f64
    } else {
        0.0
    };
    let score = average(&scores).unwrap_or(0.0).clamp(0.0, 1.0);
    let confidence = (average(&confidences).unwrap_or(0.0)
        * (evaluated_count as f64 / 5.0).clamp(0.0, 1.0))
    .clamp(0.0, 1.0);
    let passing = evaluated_count > 0 && success_rate >= 0.5 && confidence >= MIN_APPLY_CONFIDENCE;

    let mut metrics = Vec::new();
    if evaluated_count > 0 {
        metrics.push((
            ModelRouterMetricName::IntelligenceScore,
            ModelRouterMetricValue::Score(score),
        ));
        metrics.push((
            ModelRouterMetricName::SuccessRate,
            ModelRouterMetricValue::Score(success_rate),
        ));
        if let Some(median_latency_ms) = median_u64(latencies) {
            metrics.push((
                ModelRouterMetricName::MedianLatencyMs,
                ModelRouterMetricValue::Millis(median_latency_ms),
            ));
        }
        if let Some(cost) = median_i64(costs) {
            metrics.push((
                ModelRouterMetricName::EstimatedCostUsdMicros,
                ModelRouterMetricValue::UsdMicros(cost),
            ));
        }
    }

    Ok(CandidateTuneEvaluation {
        metrics,
        evaluated_count,
        skipped_count,
        confidence,
        passing,
        budget_used,
        shadow_evaluations,
    })
}

async fn record_model_router_tune_ledger_entry(
    state_db: &StateRuntime,
    task_key: &str,
    request_kind: RouterRequestKind,
    model_provider: &str,
    model: Option<String>,
    account_id: Option<String>,
    token_usage: TokenUsage,
    actual_cost_usd_micros: i64,
    price_confidence: f64,
    outcome: &str,
) {
    if let Err(err) = state_db
        .record_model_router_ledger_entry(ModelRouterLedgerEntry {
            task_key: task_key.to_string(),
            request_kind,
            model_provider: Some(model_provider.to_string()),
            model,
            account_id,
            token_usage,
            actual_cost_usd_micros,
            counterfactual_cost_usd_micros: 0,
            price_confidence,
            outcome: Some(outcome.to_string()),
        })
        .await
    {
        tracing::debug!(task_key, request_kind = request_kind.as_str(), error = %err, "failed to record model router tune ledger entry");
    }
}

fn candidate_cost_estimate(
    usage: &TokenUsage,
    candidate: &codex_config::config_toml::ModelRouterCandidateToml,
) -> (i64, f64) {
    token_price_from_candidate(candidate)
        .map(|price| {
            (
                codex_model_router::estimate_token_cost(usage, &price, /*confidence*/ 1.0)
                    .usd_micros,
                1.0,
            )
        })
        .unwrap_or((0, 0.0))
}

fn shadow_phase_for_case(
    promotions: &[ModelRouterLifecyclePromotionRecord],
    task_key: &str,
    candidate_identity: &str,
) -> &'static str {
    if promotions.iter().any(|promotion| {
        promotion.task_key == task_key
            && promotion.candidate_identity == candidate_identity
            && promotion.status.eq_ignore_ascii_case("promoted")
    }) {
        "monitoring"
    } else {
        "promotion"
    }
}

fn historical_production_identity_key(case: &ReplayCase) -> String {
    model_router_candidate_identity_key(&codex_config::config_toml::ModelRouterCandidateToml {
        id: Some("historical_production".to_string()),
        model: case.production_model.clone(),
        model_provider: Some(case.production_model_provider.clone()),
        ..Default::default()
    })
}

async fn replay_candidate_case(
    config: &Config,
    candidate: &codex_config::config_toml::ModelRouterCandidateToml,
    case: &ReplayCase,
    runtime: &ModelRouterTuneRuntime,
) -> anyhow::Result<ModelTuneTurnOutput> {
    let mut candidate_config = config.clone();
    candidate_config.model_router = None;
    apply_candidate(&mut candidate_config, candidate).map_err(anyhow::Error::msg)?;
    run_model_tune_turn(
        runtime,
        &candidate_config,
        case.user_message.clone(),
        REPLAY_BASE_INSTRUCTIONS,
        None,
    )
    .await
}

async fn judge_candidate_case(
    config: &Config,
    case: &ReplayCase,
    candidate_output: &str,
    runtime: &ModelRouterTuneRuntime,
) -> anyhow::Result<JudgeCaseEvaluation> {
    let mut judge_config = config.clone();
    if let Some(model_router) = judge_config.model_router.as_mut() {
        model_router.enabled = false;
    }
    let prompt = format!(
        "User request:\n{user}\n\nHistorical production answer:\n{historical}\n\nCandidate replay answer:\n{candidate}\n\nReturn JSON with pass, score, and confidence.",
        user = case.user_message,
        historical = case.production_output,
        candidate = candidate_output,
    );
    let output = run_model_tune_turn(
        runtime,
        &judge_config,
        prompt,
        JUDGE_BASE_INSTRUCTIONS,
        Some(judge_output_schema()),
    )
    .await?;
    let parsed = parse_judge_output(&output.text)?;
    Ok(JudgeCaseEvaluation {
        pass: parsed.pass,
        score: parsed.score.clamp(0.0, 1.0),
        confidence: parsed.confidence.clamp(0.0, 1.0),
        token_usage: output.token_usage,
    })
}

async fn run_model_tune_turn(
    runtime: &ModelRouterTuneRuntime,
    config: &Config,
    input_text: String,
    base_instructions: &str,
    output_schema: Option<Value>,
) -> anyhow::Result<ModelTuneTurnOutput> {
    let model = config
        .model
        .clone()
        .ok_or_else(|| anyhow::anyhow!("model router tune requires a configured model"))?;
    let model_info = runtime
        .models_manager
        .get_model_info(model.as_str(), &config.to_models_manager_config())
        .await;
    let conversation_id = ThreadId::new();
    let auth_manager = Some(auth_manager_for_config(config, &runtime.auth_manager));
    let client = ModelClient::new(
        auth_manager,
        conversation_id.into(),
        conversation_id,
        runtime.installation_id.clone(),
        &config.model_provider_id,
        config.model_provider.clone(),
        SessionSource::Cli,
        config.model_verbosity,
        config.features.enabled(Feature::EnableRequestCompression),
        config.features.enabled(Feature::RuntimeMetrics),
        crate::session::session::Session::build_model_client_beta_features_header(config),
    );
    let telemetry = codex_otel::SessionTelemetry::new(
        conversation_id,
        model.as_str(),
        model_info.slug.as_str(),
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        runtime.originator.clone(),
        /*log_user_prompts*/ false,
        runtime.terminal_type.clone(),
        SessionSource::Cli,
    );
    let output_schema_strict = output_schema.is_some();
    let prompt = Prompt {
        input: vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text: input_text }],
            phase: None,
        }],
        tools: Vec::new(),
        parallel_tool_calls: false,
        base_instructions: BaseInstructions {
            text: base_instructions.to_string(),
        },
        personality: None,
        output_schema,
        output_schema_strict,
    };

    let start = Instant::now();
    let mut client_session = client.new_session();
    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            &telemetry,
            config.model_reasoning_effort,
            config
                .model_reasoning_summary
                .unwrap_or(ReasoningSummaryConfig::Auto),
            config
                .service_tier
                .map(|service_tier| service_tier.request_value().to_string()),
            None,
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
    Ok(ModelTuneTurnOutput {
        text: output_text,
        token_usage,
        duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

fn add_turn_budget(
    budget_used: &mut ModelRouterTuneBudgetUsed,
    usage: &TokenUsage,
    candidate: Option<&codex_config::config_toml::ModelRouterCandidateToml>,
) {
    budget_used.tokens_used = budget_used.tokens_used.saturating_add(usage.total_tokens);
    if let Some(price) = candidate.and_then(token_price_from_candidate) {
        budget_used.cost_used_usd_micros = budget_used.cost_used_usd_micros.saturating_add(
            codex_model_router::estimate_token_cost(usage, &price, /*confidence*/ 1.0).usd_micros,
        );
    }
}

#[derive(Debug)]
struct ModelTuneTurnOutput {
    text: String,
    token_usage: TokenUsage,
    duration_ms: u64,
}

#[derive(Debug)]
struct JudgeCaseEvaluation {
    pass: bool,
    score: f64,
    confidence: f64,
    token_usage: TokenUsage,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JudgeOutput {
    pass: bool,
    score: f64,
    confidence: f64,
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

fn average(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn median_u64(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

fn median_i64(mut values: Vec<i64>) -> Option<i64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

#[cfg(test)]
mod tests {
    use codex_model_router::RouterSavings;
    use codex_state::ModelRouterUsageCoverage;
    use codex_state::ModelRouterUsageGroupBy;
    use codex_state::ModelRouterUsageQuery;
    use codex_state::ModelRouterUsageTotals;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn overhead_ledger_confidence_follows_actual_price_confidence() {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");

        record_model_router_tune_ledger_entry(
            &runtime,
            "module.repo_ci.review",
            RouterRequestKind::Judge,
            "openai",
            Some("judge-model".to_string()),
            /*account_id*/ None,
            TokenUsage::default(),
            /*actual_cost_usd_micros*/ 42,
            /*price_confidence*/ 1.0,
            "tune_judge",
        )
        .await;

        let summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms: i64::MAX,
                task_key: Some("module.repo_ci.review".to_string()),
                group_by: ModelRouterUsageGroupBy::RequestKind,
            })
            .await
            .expect("usage summary");

        assert_eq!(
            summary.totals,
            ModelRouterUsageTotals {
                request_count: 1,
                production_request_count: 0,
                overhead_request_count: 1,
                token_usage: TokenUsage::default(),
                savings: RouterSavings {
                    actual_production_cost_usd_micros: 0,
                    router_overhead_cost_usd_micros: 42,
                    counterfactual_cost_usd_micros: 0,
                    gross_savings_usd_micros: 0,
                    net_savings_usd_micros: -42,
                },
                average_price_confidence: 1.0,
                minimum_price_confidence: 1.0,
                coverage: ModelRouterUsageCoverage {
                    missing_price_rows: 0,
                    low_confidence_price_rows: 0,
                    zero_token_rows: 1,
                    production_rows_missing_actual_cost: 0,
                    production_rows_missing_counterfactual: 0,
                },
            }
        );
    }
}
