use std::collections::BTreeMap;

use anyhow::Context;
use chrono::DateTime;
use codex_model_router::CandidateMetrics;
use codex_model_router::estimate_token_cost;
use codex_protocol::protocol::TokenUsage;
use codex_state::ModelRouterMetricOverlay;
use codex_state::ModelRouterTuneResultRecord;
use codex_state::ModelRouterTuneRunRecord;
use codex_state::StateRuntime;

use crate::config::Config;
use crate::model_router::model_router_candidate_identity_key;
use crate::model_router::token_price_from_candidate;

use super::ModelRouterMetricChange;
use super::ModelRouterMetricDelta;
use super::ModelRouterMetricName;
use super::ModelRouterMetricSnapshot;
use super::ModelRouterMetricSource;
use super::ModelRouterMetricValue;
use super::ModelRouterOverlayAction;
use super::ModelRouterTuneRecommendation;
use super::ModelRouterTuneReport;
use super::parse_window;

pub(super) async fn current_metric_state(
    state_db: &StateRuntime,
    config: &Config,
) -> anyhow::Result<BTreeMap<String, Vec<ModelRouterMetricSnapshot>>> {
    let mut current = BTreeMap::new();
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(current);
    };
    for candidate in &model_router.candidates {
        let identity_key = model_router_candidate_identity_key(candidate);
        let overlay = state_db
            .lookup_model_router_metric_overlay(&identity_key)
            .await?;
        current.insert(
            identity_key,
            vec![
                metric_snapshot(
                    ModelRouterMetricName::IntelligenceScore,
                    candidate
                        .intelligence_score
                        .map(ModelRouterMetricValue::Score),
                    overlay
                        .as_ref()
                        .and_then(|overlay| overlay.intelligence_score)
                        .map(ModelRouterMetricValue::Score),
                ),
                metric_snapshot(
                    ModelRouterMetricName::SuccessRate,
                    candidate.success_rate.map(ModelRouterMetricValue::Score),
                    overlay
                        .as_ref()
                        .and_then(|overlay| overlay.success_rate)
                        .map(ModelRouterMetricValue::Score),
                ),
                metric_snapshot(
                    ModelRouterMetricName::MedianLatencyMs,
                    candidate
                        .median_latency_ms
                        .map(ModelRouterMetricValue::Millis),
                    overlay
                        .as_ref()
                        .and_then(|overlay| overlay.median_latency_ms)
                        .map(ModelRouterMetricValue::Millis),
                ),
                metric_snapshot(
                    ModelRouterMetricName::EstimatedCostUsdMicros,
                    token_price_from_candidate(candidate).map(|price| {
                        let usage = TokenUsage::default();
                        ModelRouterMetricValue::UsdMicros(
                            estimate_token_cost(&usage, &price, 1.0).usd_micros,
                        )
                    }),
                    overlay
                        .as_ref()
                        .and_then(|overlay| overlay.estimated_cost_usd_micros)
                        .map(ModelRouterMetricValue::UsdMicros),
                ),
            ],
        );
    }
    Ok(current)
}

pub(super) async fn apply_recommendation(
    state_db: &StateRuntime,
    report_id: &str,
    config_fingerprint: &str,
    recommendation: &ModelRouterTuneRecommendation,
) -> anyhow::Result<()> {
    let existing = state_db
        .lookup_model_router_metric_overlay(&recommendation.candidate_identity_key)
        .await?;
    let mut metrics = existing
        .map(|overlay| CandidateMetrics {
            intelligence_score: overlay.intelligence_score,
            success_rate: overlay.success_rate,
            median_latency_ms: overlay.median_latency_ms,
            estimated_cost_usd_micros: overlay.estimated_cost_usd_micros,
        })
        .unwrap_or_default();
    for change in recommendation
        .changes
        .iter()
        .filter(|change| change.apply_eligible)
    {
        apply_change_to_metrics(&mut metrics, change);
    }
    state_db
        .upsert_model_router_metric_overlay(ModelRouterMetricOverlay {
            candidate_identity: recommendation.candidate_identity_key.clone(),
            intelligence_score: metrics.intelligence_score,
            success_rate: metrics.success_rate,
            median_latency_ms: metrics.median_latency_ms,
            estimated_cost_usd_micros: metrics.estimated_cost_usd_micros,
            source_report_id: report_id.to_string(),
            config_fingerprint: config_fingerprint.to_string(),
        })
        .await
}

pub(super) async fn persist_report(
    state_db: &StateRuntime,
    report: &ModelRouterTuneReport,
) -> anyhow::Result<()> {
    let generated_at = DateTime::parse_from_rfc3339(&report.generated_at)
        .context("parse report generated_at")?
        .timestamp_millis();
    state_db
        .record_model_router_tune_run(ModelRouterTuneRunRecord {
            run_id: report.run_id.clone(),
            schema_version: report.schema_version,
            generated_at_ms: generated_at,
            window_start_ms: parse_window(&report.window)?.start_ms,
            window_end_ms: generated_at,
            config_fingerprint: report.config_fingerprint.clone(),
            evaluated_count: report.evaluated_count,
            skipped_count: report.skipped_count,
            cost_budget_usd_micros: report.budget.cost_budget_usd_micros,
            token_budget: report.budget.token_budget,
            cost_used_usd_micros: report.budget_used.cost_used_usd_micros,
            tokens_used: report.budget_used.tokens_used,
            report_json: Some(serde_json::to_string(report)?),
        })
        .await?;
    for recommendation in &report.recommendations {
        state_db
            .record_model_router_tune_result(ModelRouterTuneResultRecord {
                run_id: report.run_id.clone(),
                candidate_identity: recommendation.candidate_identity_key.clone(),
                task_key: "model_router.tune".to_string(),
                status: if recommendation.passing {
                    "passing".to_string()
                } else {
                    "failing".to_string()
                },
                score: Some(recommendation.confidence),
                confidence: recommendation.confidence,
                prompt_tokens: report.budget_used.tokens_used,
                completion_tokens: 0,
                total_tokens: report.budget_used.tokens_used,
                cost_usd_micros: report.budget_used.cost_used_usd_micros,
                output_json: None,
            })
            .await?;
    }
    Ok(())
}

fn metric_snapshot(
    metric: ModelRouterMetricName,
    explicit_value: Option<ModelRouterMetricValue>,
    overlay_value: Option<ModelRouterMetricValue>,
) -> ModelRouterMetricSnapshot {
    if let Some(value) = explicit_value {
        return ModelRouterMetricSnapshot {
            metric,
            source: ModelRouterMetricSource::ExplicitToml,
            value: Some(value),
        };
    }
    if let Some(value) = overlay_value {
        return ModelRouterMetricSnapshot {
            metric,
            source: ModelRouterMetricSource::AppliedOverlay,
            value: Some(value),
        };
    }
    ModelRouterMetricSnapshot {
        metric,
        source: ModelRouterMetricSource::Missing,
        value: None,
    }
}

pub(super) fn update_change_delta_and_action(change: &mut ModelRouterMetricChange) {
    change.delta = metric_delta(change.current_value, change.proposed_value);
    change.action = match (
        change.current_source,
        change.current_value,
        change.proposed_value,
    ) {
        (ModelRouterMetricSource::ExplicitToml, Some(_), _) => ModelRouterOverlayAction::Remove,
        (ModelRouterMetricSource::AppliedOverlay, Some(current), Some(proposed))
            if current == proposed =>
        {
            ModelRouterOverlayAction::Retain
        }
        (ModelRouterMetricSource::AppliedOverlay, Some(_), Some(_)) => {
            ModelRouterOverlayAction::Update
        }
        (ModelRouterMetricSource::Missing, None, Some(_)) => ModelRouterOverlayAction::Add,
        (_, _, None) => ModelRouterOverlayAction::Remove,
        (_, _, Some(_)) => ModelRouterOverlayAction::Update,
    };
}

fn apply_change_to_metrics(metrics: &mut CandidateMetrics, change: &ModelRouterMetricChange) {
    let value = if change.action == ModelRouterOverlayAction::Remove {
        None
    } else {
        change.proposed_value
    };
    match (change.metric, value) {
        (ModelRouterMetricName::IntelligenceScore, Some(ModelRouterMetricValue::Score(value))) => {
            metrics.intelligence_score = Some(value);
        }
        (ModelRouterMetricName::IntelligenceScore, None) => metrics.intelligence_score = None,
        (ModelRouterMetricName::SuccessRate, Some(ModelRouterMetricValue::Score(value))) => {
            metrics.success_rate = Some(value);
        }
        (ModelRouterMetricName::SuccessRate, None) => metrics.success_rate = None,
        (ModelRouterMetricName::MedianLatencyMs, Some(ModelRouterMetricValue::Millis(value))) => {
            metrics.median_latency_ms = Some(value);
        }
        (ModelRouterMetricName::MedianLatencyMs, None) => metrics.median_latency_ms = None,
        (
            ModelRouterMetricName::EstimatedCostUsdMicros,
            Some(ModelRouterMetricValue::UsdMicros(value)),
        ) => metrics.estimated_cost_usd_micros = Some(value),
        (ModelRouterMetricName::EstimatedCostUsdMicros, None) => {
            metrics.estimated_cost_usd_micros = None;
        }
        (ModelRouterMetricName::IntelligenceScore, Some(_))
        | (ModelRouterMetricName::SuccessRate, Some(_))
        | (ModelRouterMetricName::MedianLatencyMs, Some(_))
        | (ModelRouterMetricName::EstimatedCostUsdMicros, Some(_)) => {}
    }
}

fn metric_delta(
    current: Option<ModelRouterMetricValue>,
    proposed: Option<ModelRouterMetricValue>,
) -> Option<ModelRouterMetricDelta> {
    let (Some(current), Some(proposed)) = (current, proposed) else {
        return None;
    };
    Some(ModelRouterMetricDelta {
        absolute: metric_value_as_f64(proposed) - metric_value_as_f64(current),
    })
}

fn metric_value_as_f64(value: ModelRouterMetricValue) -> f64 {
    match value {
        ModelRouterMetricValue::Score(value) => value,
        ModelRouterMetricValue::Millis(value) => value as f64,
        ModelRouterMetricValue::UsdMicros(value) => value as f64,
    }
}
