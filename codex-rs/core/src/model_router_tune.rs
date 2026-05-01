use std::sync::Arc;

use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_login::AuthManager;
use codex_model_router::ModelRouterCandidateIdentity;
use codex_model_router::RouterTaskClass;
use codex_model_router::estimate_task_usage;
use codex_model_router::estimate_token_cost;
use codex_models_manager::manager::SharedModelsManager;
use codex_state::StateRuntime;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use uuid::Uuid;

use crate::config::Config;
use crate::model_router::model_router_candidate_identity;
use crate::model_router::model_router_candidate_identity_key;
use crate::model_router::token_price_from_candidate;

mod apply;
mod cases;
mod eval;

use cases::BudgetSelection;
use cases::ReplayCase;
use cases::collect_replay_cases;
use cases::select_budgeted_cases;

pub const MODEL_ROUTER_TUNE_REPORT_SCHEMA_VERSION: i64 = 1;

const MIN_APPLY_CONFIDENCE: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRouterTuneOptions {
    pub window: String,
    pub cost_budget_usd: f64,
    pub token_budget: i64,
    pub dry_run: bool,
}

#[derive(Clone)]
pub struct ModelRouterTuneRuntime {
    pub auth_manager: Arc<AuthManager>,
    pub models_manager: SharedModelsManager,
    pub installation_id: String,
    pub originator: String,
    pub terminal_type: String,
}

impl ModelRouterTuneRuntime {
    pub fn new(auth_manager: Arc<AuthManager>, models_manager: SharedModelsManager) -> Self {
        Self {
            auth_manager,
            models_manager,
            installation_id: Uuid::new_v4().to_string(),
            originator: "model-router-tune".to_string(),
            terminal_type: "cli".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterTuneReport {
    pub schema_version: i64,
    pub run_id: String,
    pub generated_at: String,
    pub window: String,
    pub config_fingerprint: String,
    pub evaluated_count: i64,
    pub skipped_count: i64,
    pub budget: ModelRouterTuneBudget,
    pub budget_used: ModelRouterTuneBudgetUsed,
    pub candidates: Vec<ModelRouterTuneCandidate>,
    pub current_state: Vec<ModelRouterCurrentMetricSnapshot>,
    pub recommendations: Vec<ModelRouterTuneRecommendation>,
    pub apply_eligibility: ModelRouterReportApplyEligibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterTuneBudget {
    pub cost_budget_usd_micros: i64,
    pub token_budget: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterTuneBudgetUsed {
    pub cost_used_usd_micros: i64,
    pub tokens_used: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterTuneCandidate {
    pub identity_key: String,
    pub identity: ModelRouterCandidateIdentity,
    pub model: Option<String>,
    pub model_provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterCurrentMetricSnapshot {
    pub candidate_identity_key: String,
    pub metrics: Vec<ModelRouterMetricSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterMetricSnapshot {
    pub metric: ModelRouterMetricName,
    pub source: ModelRouterMetricSource,
    pub value: Option<ModelRouterMetricValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterTuneRecommendation {
    pub candidate_identity_key: String,
    pub identity: ModelRouterCandidateIdentity,
    pub evaluated_count: i64,
    pub skipped_count: i64,
    pub confidence: f64,
    pub passing: bool,
    pub apply_eligible: bool,
    pub applied: bool,
    pub reason: Option<String>,
    pub changes: Vec<ModelRouterMetricChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterMetricChange {
    pub metric: ModelRouterMetricName,
    pub current_source: ModelRouterMetricSource,
    pub current_value: Option<ModelRouterMetricValue>,
    pub proposed_value: Option<ModelRouterMetricValue>,
    pub delta: Option<ModelRouterMetricDelta>,
    pub confidence: f64,
    pub action: ModelRouterOverlayAction,
    pub apply_eligible: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterMetricName {
    IntelligenceScore,
    SuccessRate,
    MedianLatencyMs,
    EstimatedCostUsdMicros,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterMetricSource {
    ExplicitToml,
    AppliedOverlay,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ModelRouterMetricValue {
    Score(f64),
    Millis(u64),
    UsdMicros(i64),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterMetricDelta {
    pub absolute: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouterOverlayAction {
    Add,
    Update,
    Retain,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterReportApplyEligibility {
    pub eligible: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouterReportApplyOutcome {
    pub dry_run: bool,
    pub applied_recommendations: i64,
    pub report: ModelRouterTuneReport,
}

pub async fn tune_model_router(
    state_db: &StateRuntime,
    config: &Config,
    options: ModelRouterTuneOptions,
    runtime: Option<ModelRouterTuneRuntime>,
) -> anyhow::Result<ModelRouterTuneReport> {
    let parsed_window = parse_window(&options.window)?;
    let candidates = config
        .model_router
        .as_ref()
        .map(|router| router.candidates.as_slice())
        .unwrap_or(&[]);
    let replay_cases = collect_replay_cases(state_db, parsed_window.start_ms).await?;
    let budget = ModelRouterTuneBudget {
        cost_budget_usd_micros: usd_to_micros(options.cost_budget_usd)?,
        token_budget: options.token_budget.max(0),
    };
    let selection = select_budgeted_cases(replay_cases, candidates, budget.clone());
    let mut report = build_report(
        config,
        &options.window,
        parsed_window,
        budget,
        selection,
        runtime.as_ref(),
    )
    .await?;
    refresh_model_router_report_deltas(state_db, config, &mut report).await?;
    if !options.dry_run {
        let outcome = apply_model_router_tune_report(state_db, config, report, false).await?;
        report = outcome.report;
    }
    apply::persist_report(state_db, &report).await?;
    Ok(report)
}

pub async fn refresh_model_router_report_deltas(
    state_db: &StateRuntime,
    config: &Config,
    report: &mut ModelRouterTuneReport,
) -> anyhow::Result<()> {
    let fingerprint = config_fingerprint(config)?;
    let fingerprint_matches = report.config_fingerprint == fingerprint;
    report.apply_eligibility = if fingerprint_matches {
        ModelRouterReportApplyEligibility {
            eligible: true,
            reason: None,
        }
    } else {
        ModelRouterReportApplyEligibility {
            eligible: false,
            reason: Some("config fingerprint differs from the stored report".to_string()),
        }
    };
    let current = apply::current_metric_state(state_db, config).await?;
    report.current_state = current
        .iter()
        .map(
            |(candidate_identity_key, metrics)| ModelRouterCurrentMetricSnapshot {
                candidate_identity_key: candidate_identity_key.clone(),
                metrics: metrics.clone(),
            },
        )
        .collect();
    for recommendation in &mut report.recommendations {
        let Some(current_metrics) = current.get(&recommendation.candidate_identity_key) else {
            recommendation.apply_eligible = false;
            recommendation.reason =
                Some("candidate identity is not present in current config".to_string());
            for change in &mut recommendation.changes {
                change.apply_eligible = false;
                change.reason =
                    Some("candidate identity is not present in current config".to_string());
            }
            continue;
        };
        for change in &mut recommendation.changes {
            let current_metric = current_metrics
                .iter()
                .find(|snapshot| snapshot.metric == change.metric)
                .cloned();
            if let Some(current_metric) = current_metric {
                change.current_source = current_metric.source;
                change.current_value = current_metric.value;
            }
            apply::update_change_delta_and_action(change);
            if !fingerprint_matches {
                change.apply_eligible = false;
                change.reason =
                    Some("config fingerprint differs from the stored report".to_string());
            } else if change.confidence < MIN_APPLY_CONFIDENCE {
                change.apply_eligible = false;
                change.reason = Some("confidence is below the apply threshold".to_string());
            } else if change.current_source == ModelRouterMetricSource::ExplicitToml {
                change.apply_eligible = change.action == ModelRouterOverlayAction::Remove;
                change.reason = Some("explicit config.toml metric takes precedence".to_string());
            } else {
                change.apply_eligible = true;
                change.reason = None;
            }
        }
        recommendation.apply_eligible = recommendation.passing
            && recommendation
                .changes
                .iter()
                .any(|change| change.apply_eligible);
        if !recommendation.passing {
            recommendation.reason =
                Some("recommendation did not pass confidence checks".to_string());
        } else if !recommendation.apply_eligible && recommendation.reason.is_none() {
            recommendation.reason = Some("no metric changes are eligible to apply".to_string());
        } else if recommendation.apply_eligible {
            recommendation.reason = None;
        }
    }
    Ok(())
}

pub async fn apply_model_router_tune_report(
    state_db: &StateRuntime,
    config: &Config,
    mut report: ModelRouterTuneReport,
    dry_run: bool,
) -> anyhow::Result<ModelRouterReportApplyOutcome> {
    if report.schema_version != MODEL_ROUTER_TUNE_REPORT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported model router tune report schema version {}",
            report.schema_version
        );
    }
    refresh_model_router_report_deltas(state_db, config, &mut report).await?;
    let mut applied_recommendations = 0;
    if report.apply_eligibility.eligible {
        let report_id = report.run_id.clone();
        let config_fingerprint = report.config_fingerprint.clone();
        for recommendation in &mut report.recommendations {
            if !recommendation.apply_eligible {
                continue;
            }
            if !dry_run {
                apply::apply_recommendation(
                    state_db,
                    &report_id,
                    &config_fingerprint,
                    recommendation,
                )
                .await?;
            }
            recommendation.applied = !dry_run;
            applied_recommendations += 1;
        }
    }
    Ok(ModelRouterReportApplyOutcome {
        dry_run,
        applied_recommendations,
        report,
    })
}

pub fn config_fingerprint(config: &Config) -> anyhow::Result<String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Fingerprint<'a> {
        model: &'a Option<String>,
        model_provider_id: &'a str,
        model_router: &'a Option<codex_config::config_toml::ModelRouterToml>,
    }

    let input = serde_json::to_vec(&Fingerprint {
        model: &config.model,
        model_provider_id: config.model_provider_id.as_str(),
        model_router: &config.model_router,
    })?;
    let digest = Sha256::digest(input);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Clone, Copy)]
struct ParsedWindow {
    start_ms: Option<i64>,
    end_ms: i64,
}

async fn build_report(
    config: &Config,
    window: &str,
    parsed_window: ParsedWindow,
    budget: ModelRouterTuneBudget,
    selection: BudgetSelection,
    runtime: Option<&ModelRouterTuneRuntime>,
) -> anyhow::Result<ModelRouterTuneReport> {
    let candidates = config
        .model_router
        .as_ref()
        .map(|router| router.candidates.as_slice())
        .unwrap_or(&[]);
    let fallback_evaluated_count = i64::try_from(selection.cases.len()).unwrap_or(i64::MAX);
    let fallback_confidence = (fallback_evaluated_count as f64 / 5.0).clamp(0.0, 1.0);
    let fallback_passing =
        fallback_evaluated_count > 0 && fallback_confidence >= MIN_APPLY_CONFIDENCE;
    let generated_at = DateTime::<Utc>::from_timestamp_millis(parsed_window.end_ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut recommendations = Vec::new();
    let mut actual_budget_used = ModelRouterTuneBudgetUsed {
        cost_used_usd_micros: 0,
        tokens_used: 0,
    };
    let mut used_runtime = false;
    for candidate in candidates {
        let identity_key = model_router_candidate_identity_key(candidate);
        let evaluation = if let Some(runtime) = runtime {
            match eval::evaluate_candidate(config, candidate, &selection.cases, runtime).await {
                Ok(evaluation) => {
                    used_runtime = true;
                    add_budget_used(&mut actual_budget_used, &evaluation.budget_used);
                    Some(Ok(evaluation))
                }
                Err(err) => {
                    tracing::warn!(candidate_identity = identity_key, error = %err, "model router tune evaluation failed");
                    Some(Err(err))
                }
            }
        } else {
            None
        };

        let (proposed, evaluated_count, skipped_count, confidence, passing, reason) =
            match evaluation {
                Some(Ok(evaluation)) => (
                    evaluation.metrics,
                    evaluation.evaluated_count,
                    evaluation
                        .skipped_count
                        .saturating_add(selection.skipped_count),
                    evaluation.confidence,
                    evaluation.passing,
                    None,
                ),
                Some(Err(err)) => (
                    Vec::new(),
                    0,
                    selection
                        .skipped_count
                        .saturating_add(i64::try_from(selection.cases.len()).unwrap_or(i64::MAX)),
                    0.0,
                    false,
                    Some(format!("evaluation failed: {err}")),
                ),
                None => (
                    proposed_metrics(config, candidate, &selection.cases),
                    fallback_evaluated_count,
                    selection.skipped_count,
                    fallback_confidence,
                    fallback_passing,
                    None,
                ),
            };
        recommendations.push(ModelRouterTuneRecommendation {
            candidate_identity_key: identity_key,
            identity: model_router_candidate_identity(candidate),
            evaluated_count,
            skipped_count,
            confidence,
            passing,
            apply_eligible: false,
            applied: false,
            reason,
            changes: proposed
                .into_iter()
                .map(|(metric, proposed_value)| ModelRouterMetricChange {
                    metric,
                    current_source: ModelRouterMetricSource::Missing,
                    current_value: None,
                    proposed_value: Some(proposed_value),
                    delta: None,
                    confidence,
                    action: ModelRouterOverlayAction::Add,
                    apply_eligible: false,
                    reason: None,
                })
                .collect(),
        });
    }

    Ok(ModelRouterTuneReport {
        schema_version: MODEL_ROUTER_TUNE_REPORT_SCHEMA_VERSION,
        run_id: Uuid::new_v4().to_string(),
        generated_at,
        window: window.to_string(),
        config_fingerprint: config_fingerprint(config).unwrap_or_default(),
        evaluated_count: recommendations
            .iter()
            .map(|recommendation| recommendation.evaluated_count)
            .max()
            .unwrap_or(0),
        skipped_count: selection.skipped_count,
        budget,
        budget_used: if used_runtime {
            actual_budget_used
        } else {
            selection.budget_used
        },
        candidates: candidates
            .iter()
            .map(|candidate| ModelRouterTuneCandidate {
                identity_key: model_router_candidate_identity_key(candidate),
                identity: model_router_candidate_identity(candidate),
                model: candidate.model.clone().or_else(|| config.model.clone()),
                model_provider: candidate
                    .model_provider
                    .clone()
                    .or_else(|| Some(config.model_provider_id.clone())),
            })
            .collect(),
        current_state: Vec::new(),
        recommendations,
        apply_eligibility: ModelRouterReportApplyEligibility {
            eligible: true,
            reason: None,
        },
    })
}

fn add_budget_used(total: &mut ModelRouterTuneBudgetUsed, usage: &ModelRouterTuneBudgetUsed) {
    total.cost_used_usd_micros = total
        .cost_used_usd_micros
        .saturating_add(usage.cost_used_usd_micros);
    total.tokens_used = total.tokens_used.saturating_add(usage.tokens_used);
}

fn proposed_metrics(
    config: &Config,
    candidate: &ModelRouterCandidateToml,
    cases: &[ReplayCase],
) -> Vec<(ModelRouterMetricName, ModelRouterMetricValue)> {
    if cases.is_empty() {
        return Vec::new();
    }
    let model = candidate
        .model
        .as_deref()
        .or(config.model.as_deref())
        .unwrap_or("");
    let mut metrics = vec![
        (
            ModelRouterMetricName::IntelligenceScore,
            ModelRouterMetricValue::Score(heuristic_intelligence_score(model)),
        ),
        (
            ModelRouterMetricName::SuccessRate,
            ModelRouterMetricValue::Score(1.0),
        ),
    ];
    if let Some(median_latency_ms) = median_latency_ms(cases) {
        metrics.push((
            ModelRouterMetricName::MedianLatencyMs,
            ModelRouterMetricValue::Millis(median_latency_ms),
        ));
    }
    if let Some(price) = token_price_from_candidate(candidate) {
        let mut costs = cases
            .iter()
            .map(|case| {
                let task_class = RouterTaskClass::infer(&case.task_key, case.prompt_bytes);
                estimate_token_cost(
                    &estimate_task_usage(case.prompt_bytes, task_class),
                    &price,
                    1.0,
                )
                .usd_micros
            })
            .collect::<Vec<_>>();
        costs.sort_unstable();
        if let Some(cost) = costs.get(costs.len() / 2) {
            metrics.push((
                ModelRouterMetricName::EstimatedCostUsdMicros,
                ModelRouterMetricValue::UsdMicros(*cost),
            ));
        }
    }
    metrics
}

fn parse_window(window: &str) -> anyhow::Result<ParsedWindow> {
    let end_ms = Utc::now().timestamp_millis();
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return Ok(ParsedWindow {
            start_ms: None,
            end_ms,
        });
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let value = number
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("window must be a duration like 30d, 24h, 30m, or all"))?;
    let multiplier = match unit {
        "d" => 24 * 60 * 60 * 1000,
        "h" => 60 * 60 * 1000,
        "m" => 60 * 1000,
        _ => {
            anyhow::bail!("window must be a duration like 30d, 24h, 30m, or all");
        }
    };
    Ok(ParsedWindow {
        start_ms: Some(end_ms.saturating_sub(value.max(0).saturating_mul(multiplier))),
        end_ms,
    })
}

fn usd_to_micros(usd: f64) -> anyhow::Result<i64> {
    if !usd.is_finite() || usd < 0.0 {
        anyhow::bail!("cost budget must be a non-negative finite USD amount");
    }
    Ok((usd * 1_000_000.0).round() as i64)
}

fn median_latency_ms(cases: &[ReplayCase]) -> Option<u64> {
    let mut latencies = cases
        .iter()
        .filter_map(|case| case.duration_ms)
        .filter(|latency| *latency >= 0)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    latencies
        .get(latencies.len() / 2)
        .and_then(|latency| u64::try_from(*latency).ok())
}

fn heuristic_intelligence_score(model: &str) -> f64 {
    let model = model.to_ascii_lowercase();
    if model.contains("gpt-5.5") {
        0.98
    } else if model.contains("gpt-5.4") {
        0.94
    } else if model.contains("gpt-5.3") {
        0.90
    } else if model.contains("gpt-5") {
        0.86
    } else if model.contains("gpt-4.1") || model.contains("gpt-4o") {
        0.78
    } else if model.contains("mini") {
        0.62
    } else if model.contains("spark") || model.contains("nano") {
        0.52
    } else {
        0.55
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use codex_config::config_toml::ModelRouterToml;

    use super::*;
    use crate::config;

    #[tokio::test]
    async fn report_apply_persists_only_high_confidence_changes() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("fast".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let candidate = config
            .model_router
            .as_ref()
            .expect("router")
            .candidates
            .first()
            .expect("candidate");
        let identity_key = model_router_candidate_identity_key(candidate);
        let mut report = base_report(&config, &identity_key);
        report.recommendations[0].confidence = 0.4;
        report.recommendations[0].passing = false;
        report.recommendations[0].changes[0].confidence = 0.4;

        let outcome = apply_model_router_tune_report(&runtime, &config, report, false)
            .await
            .expect("apply report");

        assert_eq!(outcome.applied_recommendations, 0);
        assert_eq!(
            runtime
                .lookup_model_router_metric_overlay(&identity_key)
                .await
                .expect("lookup overlay"),
            None
        );
    }

    #[tokio::test]
    async fn report_apply_writes_passing_overlay_and_dry_run_does_not() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("fast".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let identity_key = model_router_candidate_identity_key(
            config
                .model_router
                .as_ref()
                .expect("router")
                .candidates
                .first()
                .expect("candidate"),
        );
        let report = base_report(&config, &identity_key);

        let dry_run = apply_model_router_tune_report(&runtime, &config, report.clone(), true)
            .await
            .expect("dry-run apply");
        assert_eq!(dry_run.applied_recommendations, 1);
        assert_eq!(
            runtime
                .lookup_model_router_metric_overlay(&identity_key)
                .await
                .expect("lookup overlay"),
            None
        );

        let applied = apply_model_router_tune_report(&runtime, &config, report, false)
            .await
            .expect("apply");
        assert_eq!(applied.applied_recommendations, 1);
        let overlay = runtime
            .lookup_model_router_metric_overlay(&identity_key)
            .await
            .expect("lookup overlay")
            .expect("overlay");
        assert_eq!(overlay.intelligence_score, Some(0.9));
    }

    #[tokio::test]
    async fn config_mismatch_previews_but_refuses_apply() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("fast".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let identity_key = model_router_candidate_identity_key(
            config
                .model_router
                .as_ref()
                .expect("router")
                .candidates
                .first()
                .expect("candidate"),
        );
        let mut report = base_report(&config, &identity_key);
        report.config_fingerprint = "different".to_string();

        let outcome = apply_model_router_tune_report(&runtime, &config, report, false)
            .await
            .expect("apply mismatch");

        assert_eq!(outcome.applied_recommendations, 0);
        assert_eq!(outcome.report.apply_eligibility.eligible, false);
        assert_eq!(
            runtime
                .lookup_model_router_metric_overlay(&identity_key)
                .await
                .expect("lookup overlay"),
            None
        );
    }

    #[test]
    fn report_serializes_with_schema_version() {
        let report = ModelRouterTuneReport {
            schema_version: MODEL_ROUTER_TUNE_REPORT_SCHEMA_VERSION,
            run_id: "run".to_string(),
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            window: "30d".to_string(),
            config_fingerprint: "fingerprint".to_string(),
            evaluated_count: 0,
            skipped_count: 0,
            budget: ModelRouterTuneBudget {
                cost_budget_usd_micros: 1,
                token_budget: 2,
            },
            budget_used: ModelRouterTuneBudgetUsed {
                cost_used_usd_micros: 0,
                tokens_used: 0,
            },
            candidates: Vec::new(),
            current_state: Vec::new(),
            recommendations: Vec::new(),
            apply_eligibility: ModelRouterReportApplyEligibility {
                eligible: true,
                reason: None,
            },
        };

        let json = serde_json::to_string(&report).expect("serialize report");
        let deserialized: ModelRouterTuneReport =
            serde_json::from_str(&json).expect("deserialize report");

        assert_eq!(deserialized, report);
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }

    fn base_report(config: &Config, identity_key: &str) -> ModelRouterTuneReport {
        ModelRouterTuneReport {
            schema_version: MODEL_ROUTER_TUNE_REPORT_SCHEMA_VERSION,
            run_id: "run".to_string(),
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            window: "all".to_string(),
            config_fingerprint: config_fingerprint(config).expect("fingerprint"),
            evaluated_count: 5,
            skipped_count: 0,
            budget: ModelRouterTuneBudget {
                cost_budget_usd_micros: 10_000_000,
                token_budget: 1_000_000,
            },
            budget_used: ModelRouterTuneBudgetUsed {
                cost_used_usd_micros: 0,
                tokens_used: 1_000,
            },
            candidates: Vec::new(),
            current_state: Vec::new(),
            recommendations: vec![ModelRouterTuneRecommendation {
                candidate_identity_key: identity_key.to_string(),
                identity: config
                    .model_router
                    .as_ref()
                    .map(|router| model_router_candidate_identity(&router.candidates[0]))
                    .expect("identity"),
                evaluated_count: 5,
                skipped_count: 0,
                confidence: 1.0,
                passing: true,
                apply_eligible: false,
                applied: false,
                reason: None,
                changes: vec![ModelRouterMetricChange {
                    metric: ModelRouterMetricName::IntelligenceScore,
                    current_source: ModelRouterMetricSource::Missing,
                    current_value: None,
                    proposed_value: Some(ModelRouterMetricValue::Score(0.9)),
                    delta: None,
                    confidence: 1.0,
                    action: ModelRouterOverlayAction::Add,
                    apply_eligible: false,
                    reason: None,
                }],
            }],
            apply_eligibility: ModelRouterReportApplyEligibility {
                eligible: true,
                reason: None,
            },
        }
    }
}
