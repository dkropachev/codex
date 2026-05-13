use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

mod auto_candidates;
mod failover;

use chrono::Utc;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_config::config_toml::ModelRouterDiscoveryToml;
use codex_login::AuthManager;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_router::CandidateMetrics;
use codex_model_router::CandidateRoute;
use codex_model_router::CostEstimate;
use codex_model_router::ModelRouterCandidateIdentity;
use codex_model_router::RouterRequestKind;
use codex_model_router::RouterTaskClass;
use codex_model_router::TokenPrice;
use codex_model_router::estimate_task_usage;
use codex_model_router::estimate_token_cost;
use codex_model_router::policy;
use codex_model_router::policy::PolicyAvailableModel;
use codex_model_router::policy::PolicyRoute;
use codex_model_router::select_candidate_with_score_bias;
use codex_models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::config_types::ModeKind;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TokenUsage;
use codex_state::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED;
use codex_state::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED;
use codex_state::MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO;
use codex_state::ModelRouterLedgerEntry;
use codex_state::ModelRouterLifecycleEventRecord;
use codex_state::ModelRouterLifecyclePromotionRecord;
use codex_state::ModelRouterLifecycleTransitionContext;
use codex_state::ModelRouterMetricOverlay;
use codex_state::ModelRouterShadowEvaluationSummary;
use codex_state::StateRuntime;
use tokio::sync::Mutex;

use crate::client::ModelClient;
use crate::config::Config;
use crate::config::ModelRouterAccounting;

pub(crate) use failover::ModelRouterAppliedRoute;
pub(crate) use failover::ModelRouterRouteExclusion;
pub(crate) use failover::model_router_failure_scope;
use failover::selectable_routes;

const LIFECYCLE_PHASE_PROMOTION: &str = "promotion";
const LIFECYCLE_PHASE_MONITORING: &str = "monitoring";
const LIFECYCLE_STATUS_PROMOTED: &str = MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED;
const ADDITIONAL_PROVIDER_DISCOVERY_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelRouterSource {
    Chat(ModeKind),
    SubAgent(SubAgentSource),
    Module(&'static str),
}

impl ModelRouterSource {
    pub(crate) fn task_key(&self) -> String {
        match self {
            ModelRouterSource::Chat(mode) => {
                let suffix = match mode {
                    ModeKind::Plan => "plan",
                    ModeKind::Codex | ModeKind::CodexConfigEdit => "codex",
                    ModeKind::Workflow => "workflow",
                    ModeKind::Default | ModeKind::PairProgramming | ModeKind::Execute => "default",
                };
                format!("chat.{suffix}")
            }
            ModelRouterSource::SubAgent(source) => {
                let suffix = match source {
                    SubAgentSource::Review => "review".to_string(),
                    SubAgentSource::Compact => "compact".to_string(),
                    SubAgentSource::MemoryConsolidation => "memory_consolidation".to_string(),
                    SubAgentSource::ThreadSpawn { agent_role, .. } => agent_role
                        .as_ref()
                        .map(|role| format!("thread_spawn.{role}"))
                        .unwrap_or_else(|| "thread_spawn".to_string()),
                    SubAgentSource::Other(source) => source.clone(),
                };
                format!("subagent.{suffix}")
            }
            ModelRouterSource::Module(module) => format!("module.{module}"),
        }
    }
}

#[cfg(test)]
pub(crate) fn apply_model_router(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
) -> Result<(), String> {
    apply_model_router_with_exclusions(config, source, prompt_bytes, available_models, &[])?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_model_router_with_exclusions(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    exclusions: &[ModelRouterRouteExclusion],
) -> Result<Option<ModelRouterAppliedRoute>, String> {
    apply_model_router_with_overlays_and_exclusions(
        config,
        source,
        prompt_bytes,
        available_models,
        &[],
        &[],
        exclusions,
        /*enforce_lifecycle*/ false,
    )
}

pub(crate) async fn apply_model_router_with_state(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    state_db: Option<&StateRuntime>,
) -> Result<(), String> {
    apply_model_router_with_state_and_exclusions(
        config,
        source,
        prompt_bytes,
        available_models,
        state_db,
        &[],
    )
    .await?;
    Ok(())
}

pub(crate) async fn apply_model_router_with_state_and_exclusions(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    state_db: Option<&StateRuntime>,
    exclusions: &[ModelRouterRouteExclusion],
) -> Result<Option<ModelRouterAppliedRoute>, String> {
    let overlays = load_metric_overlays(config, state_db).await;
    let task_key = source.task_key();
    let promotions = load_lifecycle_promotions(&task_key, state_db).await;
    let promotions = apply_lifecycle_transitions(
        LifecycleTransitionInputs {
            config,
            task_key: &task_key,
            prompt_bytes,
            available_models,
            overlays: &overlays,
            state_db,
            exclusions,
        },
        promotions,
    )
    .await;
    apply_model_router_with_overlays_and_exclusions(
        config,
        source,
        prompt_bytes,
        available_models,
        &overlays,
        &promotions,
        exclusions,
        /*enforce_lifecycle*/ state_db.is_some(),
    )
}

fn apply_model_router_with_overlays_and_exclusions(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    overlays: &[ModelRouterMetricOverlay],
    promotions: &[ModelRouterLifecyclePromotionRecord],
    exclusions: &[ModelRouterRouteExclusion],
    enforce_lifecycle: bool,
) -> Result<Option<ModelRouterAppliedRoute>, String> {
    config.model_router_accounting = None;
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(None);
    };
    if !model_router.enabled {
        return Ok(None);
    }

    let task_key = source.task_key();
    let candidate_set =
        build_candidate_set(config, &task_key, prompt_bytes, available_models, overlays)?;
    tracing::debug!(
        task_key = task_key,
        candidates = model_router.candidates.len(),
        auto_candidates = candidate_set.auto_candidate_count,
        "evaluating model router"
    );

    let selectable_routes = selectable_routes(config, &candidate_set, exclusions);
    let policy_routes = selectable_routes
        .iter()
        .enumerate()
        .map(|(index, route)| PolicyRoute {
            index,
            model_provider: route.key.model_provider.clone(),
            model: route.key.model.clone(),
        })
        .collect::<Vec<_>>();
    let policy_application =
        policy::apply_model_router_policy(model_router, &task_key, &policy_routes)
            .map_err(|err| err.to_string())?;
    let filtered_routes = policy_application
        .routes
        .iter()
        .filter_map(|decision| selectable_routes.get(decision.route_index))
        .map(|route| route.route.clone())
        .collect::<Vec<_>>();
    let score_biases = policy_application
        .routes
        .iter()
        .map(|decision| decision.score_bias)
        .collect::<Vec<_>>();
    let selected_route_index = promoted_policy_route_index(
        &task_key,
        &candidate_set,
        &selectable_routes,
        &policy_application,
        promotions,
    )
    .or_else(|| {
        select_lifecycle_production_route_index(
            model_router,
            &task_key,
            prompt_bytes,
            &candidate_set,
            &selectable_routes,
            &policy_routes,
            &policy_application,
            &filtered_routes,
            &score_biases,
            enforce_lifecycle,
        )
    });
    let Some(selected_route_index) = selected_route_index else {
        return Ok(None);
    };
    let selected_route = policy_application
        .routes
        .get(selected_route_index)
        .and_then(|decision| selectable_routes.get(decision.route_index))
        .ok_or_else(|| {
            format!("model_router selected missing filtered route index {selected_route_index}")
        })?;
    let selected_index = selected_route.index;
    tracing::debug!(
        task_key = task_key,
        selected_index,
        "selected model router candidate"
    );
    let applied_route = ModelRouterAppliedRoute {
        task_key: task_key.clone(),
        route: selected_route.key.clone(),
    };
    let accounting = build_model_router_accounting(
        config,
        &task_key,
        selected_index,
        &selected_route.key,
        &candidate_set,
    );
    if selected_index == 0 {
        config.model_router_accounting = Some(accounting);
        return Ok(Some(applied_route));
    }
    let Some(candidate) = candidate_set.candidates.get(selected_index - 1) else {
        return Err(format!(
            "model_router selected missing candidate index {selected_index}"
        ));
    };
    let mut router_config = config.clone();
    apply_candidate(&mut router_config, candidate)?;
    router_config.model_router_accounting = Some(accounting);
    *config = router_config;
    Ok(Some(applied_route))
}

pub(crate) async fn record_model_router_request_usage(
    state_db: Option<&StateRuntime>,
    accounting: Option<&ModelRouterAccounting>,
    token_usage: &TokenUsage,
    outcome: &str,
) {
    let Some(state_db) = state_db else {
        return;
    };
    let Some(accounting) = accounting else {
        return;
    };

    let actual = cost_estimate_for_price(
        token_usage,
        accounting.actual_price.as_ref(),
        accounting.actual_price_confidence,
    );
    let counterfactual = cost_estimate_for_price(
        token_usage,
        accounting.counterfactual_price.as_ref(),
        accounting.counterfactual_price_confidence,
    );
    let price_confidence = actual.confidence.min(counterfactual.confidence);
    if let Err(err) = state_db
        .record_model_router_ledger_entry(ModelRouterLedgerEntry {
            task_key: accounting.task_key.clone(),
            request_kind: RouterRequestKind::Production,
            model_provider: Some(accounting.model_provider.clone()),
            model: accounting.model.clone(),
            account_id: accounting.account_id.clone(),
            token_usage: token_usage.clone(),
            actual_cost_usd_micros: actual.usd_micros,
            counterfactual_cost_usd_micros: counterfactual.usd_micros,
            price_confidence,
            outcome: Some(outcome.to_string()),
        })
        .await
    {
        tracing::debug!(task_key = %accounting.task_key, error = %err, "failed to record model router production ledger entry");
    }
}

struct LifecycleTransitionInputs<'a> {
    config: &'a Config,
    task_key: &'a str,
    prompt_bytes: usize,
    available_models: &'a [AvailableRouterModel],
    overlays: &'a [ModelRouterMetricOverlay],
    state_db: Option<&'a StateRuntime>,
    exclusions: &'a [ModelRouterRouteExclusion],
}

async fn apply_lifecycle_transitions(
    inputs: LifecycleTransitionInputs<'_>,
    promotions: Vec<ModelRouterLifecyclePromotionRecord>,
) -> Vec<ModelRouterLifecyclePromotionRecord> {
    let LifecycleTransitionInputs {
        config,
        task_key,
        prompt_bytes,
        available_models,
        overlays,
        state_db,
        exclusions,
    } = inputs;
    let Some(state_db) = state_db else {
        return promotions;
    };
    let Some(model_router) = config.model_router.as_ref() else {
        return promotions;
    };
    if !model_router.enabled {
        return promotions;
    }
    let candidate_set = match build_candidate_set(
        config,
        task_key,
        prompt_bytes,
        available_models,
        overlays,
    ) {
        Ok(candidate_set) => candidate_set,
        Err(err) => {
            tracing::debug!(task_key, error = %err, "failed to build model router candidate set for lifecycle transitions");
            return promotions;
        }
    };
    let selectable_routes = selectable_routes(config, &candidate_set, exclusions);
    let policy_routes = selectable_routes
        .iter()
        .enumerate()
        .map(|(index, route)| PolicyRoute {
            index,
            model_provider: route.key.model_provider.clone(),
            model: route.key.model.clone(),
        })
        .collect::<Vec<_>>();
    let policy_application = match policy::apply_model_router_policy(
        model_router,
        task_key,
        &policy_routes,
    ) {
        Ok(policy_application) => policy_application,
        Err(err) => {
            tracing::debug!(task_key, error = %err, "failed to apply model router policy for lifecycle transitions");
            return promotions;
        }
    };

    let mut changed = false;
    for decision in &policy_application.routes {
        let Some(selectable_route) = selectable_routes.get(decision.route_index) else {
            continue;
        };
        let Some(candidate) = candidate_for_selectable_route(&candidate_set, selectable_route)
        else {
            continue;
        };
        let Some(route) = policy_routes.get(decision.route_index) else {
            continue;
        };
        let candidate_identity = model_router_candidate_identity_key(candidate);
        let lifecycle = match policy::effective_lifecycle_for_route(
            Some(model_router),
            task_key,
            Some(route),
        ) {
            Ok(lifecycle) => lifecycle,
            Err(err) => {
                tracing::debug!(task_key, candidate_identity, error = %err, "failed to resolve model router lifecycle");
                continue;
            }
        };
        if !lifecycle.shadow_allowed {
            continue;
        }
        let window_start_ms = lifecycle_window_start_ms(&lifecycle.window);
        let summaries = match state_db
            .model_router_shadow_evaluation_summaries_since(Some(task_key), window_start_ms)
            .await
        {
            Ok(summaries) => summaries,
            Err(err) => {
                tracing::debug!(task_key, candidate_identity, error = %err, "failed to load model router shadow summaries");
                continue;
            }
        };
        if promotions.iter().any(|promotion| {
            promotion.task_key == task_key
                && promotion.candidate_identity == candidate_identity
                && promotion
                    .status
                    .eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED)
        }) {
            if let Some(summary) =
                lifecycle_monitoring_failed_summary(&summaries, &candidate_identity, &lifecycle)
                && lifecycle.auto_demote
            {
                match state_db
                    .demote_model_router_lifecycle_promotion_with_event(
                        task_key,
                        &candidate_identity,
                        Some("monitoring shadow gates failed"),
                        ModelRouterLifecycleTransitionContext {
                            source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                            lifecycle_window: Some(lifecycle.window.clone()),
                            shadow_phase: Some(LIFECYCLE_PHASE_MONITORING.to_string()),
                            shadow_summary: Some(summary.clone()),
                            failed_gates_json: lifecycle_failed_gates_json(summary, &lifecycle),
                        },
                    )
                    .await
                {
                    Ok(rows) => changed |= rows > 0,
                    Err(err) => {
                        tracing::debug!(task_key, candidate_identity, error = %err, "failed to demote model router lifecycle promotion");
                    }
                }
            }
            continue;
        }
        if !lifecycle.auto_promote {
            continue;
        }
        let Some(summary) =
            matching_lifecycle_summary(&summaries, LIFECYCLE_PHASE_PROMOTION, &candidate_identity)
        else {
            continue;
        };
        if !lifecycle_summary_has_enough_samples(summary, &lifecycle) {
            continue;
        }
        if !lifecycle_summary_passes_gates(summary, &lifecycle) {
            record_model_router_lifecycle_promotion_blocked(
                state_db,
                task_key,
                &candidate_identity,
                selectable_route,
                summary,
                &lifecycle,
                &promotions,
            )
            .await;
            continue;
        }
        let now_ms = Utc::now().timestamp_millis();
        let (base_model_provider, base_model) =
            model_provider_and_model_from_identity_key(&summary.base_candidate_identity);
        match state_db
            .promote_model_router_lifecycle_promotion(
                ModelRouterLifecyclePromotionRecord {
                    task_key: task_key.to_string(),
                    candidate_identity: candidate_identity.clone(),
                    base_candidate_identity: summary.base_candidate_identity.clone(),
                    status: LIFECYCLE_STATUS_PROMOTED.to_string(),
                    rule_id: lifecycle.matched_rule_ids.first().cloned(),
                    production_model_provider: Some(selectable_route.key.model_provider.clone()),
                    production_model: selectable_route.key.model.clone(),
                    base_model_provider,
                    base_model,
                    promoted_at_ms: now_ms,
                    updated_at_ms: now_ms,
                    reason: Some("promotion shadow gates passed".to_string()),
                },
                ModelRouterLifecycleTransitionContext {
                    source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                    lifecycle_window: Some(lifecycle.window.clone()),
                    shadow_phase: Some(LIFECYCLE_PHASE_PROMOTION.to_string()),
                    shadow_summary: Some(summary.clone()),
                    failed_gates_json: None,
                },
            )
            .await
        {
            Ok(()) => changed = true,
            Err(err) => {
                tracing::debug!(task_key, candidate_identity, error = %err, "failed to promote model router lifecycle candidate");
            }
        }
    }

    if changed {
        load_lifecycle_promotions(task_key, Some(state_db)).await
    } else {
        promotions
    }
}

fn select_lifecycle_production_route_index(
    model_router: &codex_config::config_toml::ModelRouterToml,
    task_key: &str,
    prompt_bytes: usize,
    candidate_set: &CandidateSet,
    selectable_routes: &[failover::SelectableRoute],
    policy_routes: &[PolicyRoute],
    policy_application: &policy::PolicyApplication,
    filtered_routes: &[CandidateRoute],
    score_biases: &[f64],
    enforce_lifecycle: bool,
) -> Option<usize> {
    if !enforce_lifecycle {
        return select_candidate_with_score_bias(
            task_key,
            prompt_bytes,
            filtered_routes,
            score_biases,
        )
        .map(|selection| selection.index);
    }

    let mut production_routes = Vec::new();
    let mut production_score_biases = Vec::new();
    let mut production_policy_indices = Vec::new();
    for (policy_index, decision) in policy_application.routes.iter().enumerate() {
        let Some(selectable_route) = selectable_routes.get(decision.route_index) else {
            continue;
        };
        let Some(route) = policy_routes.get(decision.route_index) else {
            continue;
        };
        let Some(candidate_route) = filtered_routes.get(policy_index) else {
            continue;
        };
        if !candidate_route.is_incumbent
            && !candidate_route_is_lifecycle_exempt(candidate_set, selectable_route.index)
            && route_requires_lifecycle_promotion(model_router, task_key, route)
        {
            continue;
        }
        production_routes.push(candidate_route.clone());
        production_score_biases.push(*score_biases.get(policy_index).unwrap_or(&0.0));
        production_policy_indices.push(policy_index);
    }

    if production_routes.is_empty() {
        return select_candidate_with_score_bias(
            task_key,
            prompt_bytes,
            filtered_routes,
            score_biases,
        )
        .map(|selection| selection.index);
    }

    select_candidate_with_score_bias(
        task_key,
        prompt_bytes,
        &production_routes,
        &production_score_biases,
    )
    .and_then(|selection| production_policy_indices.get(selection.index).copied())
}

fn route_requires_lifecycle_promotion(
    model_router: &codex_config::config_toml::ModelRouterToml,
    task_key: &str,
    route: &PolicyRoute,
) -> bool {
    policy::effective_lifecycle_for_route(Some(model_router), task_key, Some(route))
        .map(|lifecycle| lifecycle.shadow_allowed)
        .unwrap_or(false)
}

fn candidate_route_is_lifecycle_exempt(candidate_set: &CandidateSet, route_index: usize) -> bool {
    let Some(candidate_index) = route_index.checked_sub(1) else {
        return false;
    };
    let exempt_start = candidate_set
        .candidates
        .len()
        .saturating_sub(candidate_set.lifecycle_exempt_candidate_count);
    candidate_index >= exempt_start && candidate_index < candidate_set.candidates.len()
}

fn candidate_for_selectable_route<'a>(
    candidate_set: &'a CandidateSet,
    route: &failover::SelectableRoute,
) -> Option<&'a ModelRouterCandidateToml> {
    route
        .index
        .checked_sub(1)
        .and_then(|candidate_index| candidate_set.candidates.get(candidate_index))
}

fn matching_lifecycle_summary<'a>(
    summaries: &'a [ModelRouterShadowEvaluationSummary],
    phase: &str,
    candidate_identity: &str,
) -> Option<&'a ModelRouterShadowEvaluationSummary> {
    summaries
        .iter()
        .find(|summary| summary.phase == phase && summary.candidate_identity == candidate_identity)
}

fn lifecycle_summary_passes_gates(
    summary: &ModelRouterShadowEvaluationSummary,
    lifecycle: &policy::EffectiveLifecycle,
) -> bool {
    lifecycle_summary_has_enough_samples(summary, lifecycle)
        && summary.average_confidence >= lifecycle.min_confidence
        && summary.success_rate >= lifecycle.min_success_rate
        && summary.cost_used_usd_micros <= lifecycle_cost_budget_usd_micros(lifecycle)
        && summary.tokens_used <= i64::try_from(lifecycle.token_budget).unwrap_or(i64::MAX)
}

fn lifecycle_summary_has_enough_samples(
    summary: &ModelRouterShadowEvaluationSummary,
    lifecycle: &policy::EffectiveLifecycle,
) -> bool {
    summary.evaluated_count >= i64::try_from(lifecycle.min_evaluated).unwrap_or(i64::MAX)
}

fn lifecycle_monitoring_failed_summary<'a>(
    summaries: &'a [ModelRouterShadowEvaluationSummary],
    candidate_identity: &str,
    lifecycle: &policy::EffectiveLifecycle,
) -> Option<&'a ModelRouterShadowEvaluationSummary> {
    let Some(summary) =
        matching_lifecycle_summary(summaries, LIFECYCLE_PHASE_MONITORING, candidate_identity)
    else {
        return None;
    };
    (lifecycle_summary_has_enough_samples(summary, lifecycle)
        && !lifecycle_summary_passes_gates(summary, lifecycle))
    .then_some(summary)
}

async fn record_model_router_lifecycle_promotion_blocked(
    state_db: &StateRuntime,
    task_key: &str,
    candidate_identity: &str,
    selectable_route: &failover::SelectableRoute,
    summary: &ModelRouterShadowEvaluationSummary,
    lifecycle: &policy::EffectiveLifecycle,
    promotions: &[ModelRouterLifecyclePromotionRecord],
) {
    let (base_model_provider, base_model) =
        model_provider_and_model_from_identity_key(&summary.base_candidate_identity);
    let current_status = promotions
        .iter()
        .find(|promotion| {
            promotion.task_key == task_key && promotion.candidate_identity == candidate_identity
        })
        .map(|promotion| promotion.status.clone());
    let mut event = ModelRouterLifecycleEventRecord {
        id: None,
        created_at_ms: Utc::now().timestamp_millis(),
        event_type: MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED.to_string(),
        source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
        task_key: task_key.to_string(),
        candidate_identity: candidate_identity.to_string(),
        base_candidate_identity: summary.base_candidate_identity.clone(),
        previous_status: current_status.clone(),
        next_status: current_status,
        rule_id: lifecycle.matched_rule_ids.first().cloned(),
        reason: Some("promotion shadow gates failed".to_string()),
        production_model_provider: Some(selectable_route.key.model_provider.clone()),
        production_model: selectable_route.key.model.clone(),
        base_model_provider,
        base_model,
        lifecycle_window: Some(lifecycle.window.clone()),
        shadow_phase: None,
        shadow_evaluated_count: None,
        shadow_success_count: None,
        shadow_success_rate: None,
        shadow_average_score: None,
        shadow_average_confidence: None,
        shadow_cost_used_usd_micros: None,
        shadow_tokens_used: None,
        shadow_latest_evaluation_id: None,
        shadow_latest_evaluation_at_ms: None,
        failed_gates_json: lifecycle_failed_gates_json(summary, lifecycle),
    };
    event.apply_shadow_summary(LIFECYCLE_PHASE_PROMOTION, summary);
    if let Err(err) = state_db
        .record_model_router_lifecycle_event_once(event)
        .await
    {
        tracing::debug!(task_key, candidate_identity, error = %err, "failed to record blocked model router lifecycle promotion");
    }
}

fn lifecycle_failed_gates_json(
    summary: &ModelRouterShadowEvaluationSummary,
    lifecycle: &policy::EffectiveLifecycle,
) -> Option<String> {
    let mut failed_gates = Vec::new();
    if summary.average_confidence < lifecycle.min_confidence {
        failed_gates.push(serde_json::json!({
            "gate": "min_confidence",
            "actual": summary.average_confidence,
            "threshold": lifecycle.min_confidence,
        }));
    }
    if summary.success_rate < lifecycle.min_success_rate {
        failed_gates.push(serde_json::json!({
            "gate": "min_success_rate",
            "actual": summary.success_rate,
            "threshold": lifecycle.min_success_rate,
        }));
    }
    let cost_budget_usd_micros = lifecycle_cost_budget_usd_micros(lifecycle);
    if summary.cost_used_usd_micros > cost_budget_usd_micros {
        failed_gates.push(serde_json::json!({
            "gate": "cost_budget_usd_micros",
            "actual": summary.cost_used_usd_micros,
            "threshold": cost_budget_usd_micros,
        }));
    }
    let token_budget = i64::try_from(lifecycle.token_budget).unwrap_or(i64::MAX);
    if summary.tokens_used > token_budget {
        failed_gates.push(serde_json::json!({
            "gate": "token_budget",
            "actual": summary.tokens_used,
            "threshold": token_budget,
        }));
    }
    (!failed_gates.is_empty()).then(|| serde_json::to_string(&failed_gates).ok())?
}

fn lifecycle_cost_budget_usd_micros(lifecycle: &policy::EffectiveLifecycle) -> i64 {
    let micros = lifecycle.cost_budget_usd * 1_000_000.0;
    if !micros.is_finite() || micros < 0.0 {
        return 0;
    }
    if micros >= i64::MAX as f64 {
        i64::MAX
    } else {
        micros.round() as i64
    }
}

fn lifecycle_window_start_ms(window: &str) -> Option<i64> {
    let window = window.trim();
    if window.eq_ignore_ascii_case("all") || window.eq_ignore_ascii_case("all-time") {
        return None;
    }
    let (number, unit) = window.split_at(window.len().saturating_sub(1));
    let value = number.parse::<i64>().ok()?.max(0);
    let multiplier = match unit {
        "d" => 24 * 60 * 60 * 1000,
        "h" => 60 * 60 * 1000,
        "m" => 60 * 1000,
        _ => return None,
    };
    Some(
        Utc::now()
            .timestamp_millis()
            .saturating_sub(value.saturating_mul(multiplier)),
    )
}

fn model_provider_and_model_from_identity_key(
    identity_key: &str,
) -> (Option<String>, Option<String>) {
    serde_json::from_str::<ModelRouterCandidateIdentity>(identity_key)
        .map(|identity| (identity.model_provider, identity.model))
        .unwrap_or((None, None))
}

async fn load_metric_overlays(
    config: &Config,
    state_db: Option<&StateRuntime>,
) -> Vec<ModelRouterMetricOverlay> {
    let Some(state_db) = state_db else {
        return Vec::new();
    };
    let Some(model_router) = config.model_router.as_ref() else {
        return Vec::new();
    };
    let mut overlays = Vec::new();
    for candidate in &model_router.candidates {
        let identity = model_router_candidate_identity_key(candidate);
        match state_db.lookup_model_router_metric_overlay(&identity).await {
            Ok(Some(overlay)) => overlays.push(overlay),
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(candidate_identity = identity, error = %err, "failed to load model router metric overlay");
            }
        }
    }
    overlays
}

async fn load_lifecycle_promotions(
    task_key: &str,
    state_db: Option<&StateRuntime>,
) -> Vec<ModelRouterLifecyclePromotionRecord> {
    let Some(state_db) = state_db else {
        return Vec::new();
    };
    match state_db
        .model_router_lifecycle_promotions(Some(task_key))
        .await
    {
        Ok(promotions) => promotions,
        Err(err) => {
            tracing::debug!(task_key, error = %err, "failed to load model router lifecycle promotions");
            Vec::new()
        }
    }
}

fn promoted_policy_route_index(
    task_key: &str,
    candidate_set: &CandidateSet,
    selectable_routes: &[failover::SelectableRoute],
    policy_application: &policy::PolicyApplication,
    promotions: &[ModelRouterLifecyclePromotionRecord],
) -> Option<usize> {
    for promotion in promotions.iter().filter(|promotion| {
        promotion.task_key == task_key && promotion.status.eq_ignore_ascii_case("promoted")
    }) {
        for (candidate_index, candidate) in candidate_set.candidates.iter().enumerate() {
            if model_router_candidate_identity_key(candidate) != promotion.candidate_identity {
                continue;
            }
            let candidate_route_index = candidate_index + 1;
            let policy_route_index = policy_application.routes.iter().position(|decision| {
                selectable_routes
                    .get(decision.route_index)
                    .is_some_and(|route| route.index == candidate_route_index)
            });
            if let Some(policy_route_index) = policy_route_index {
                return Some(policy_route_index);
            }
        }
    }
    None
}

pub(crate) fn auth_manager_for_config(
    config: &Config,
    parent: &Arc<AuthManager>,
) -> Arc<AuthManager> {
    if config_account_pool_default(config) == parent.default_account_pool_id() {
        return Arc::clone(parent);
    }
    AuthManager::shared_from_config_with_parent_auth(config, parent)
}

pub(crate) fn model_client_for_config(
    config: &Config,
    parent: &ModelClient,
    parent_auth_manager: &Arc<AuthManager>,
) -> ModelClient {
    parent.with_provider_info(
        &config.model_provider_id,
        config.model_provider.clone(),
        Some(auth_manager_for_config(config, parent_auth_manager)),
    )
}

pub(crate) async fn record_model_router_request_usage_for_config(
    state_db: Option<&StateRuntime>,
    config: &Config,
    token_usage: &TokenUsage,
    outcome: &str,
) {
    record_model_router_request_usage(
        state_db,
        config.model_router_accounting.as_ref(),
        token_usage,
        outcome,
    )
    .await;
}

pub(crate) fn config_account_pool_default(config: &Config) -> Option<String> {
    let account_pool = config.account_pool.as_ref()?;
    if !account_pool.enabled {
        return None;
    }
    account_pool
        .default_pool
        .clone()
        .or_else(|| account_pool.pools.keys().next().cloned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AvailableRouterModel {
    model_provider_id: String,
    model: String,
    context_window: Option<i64>,
    max_context_window: Option<i64>,
    effective_context_window_percent: i64,
}

impl AvailableRouterModel {
    fn from_model_info(model_provider_id: &str, model_info: &ModelInfo) -> Self {
        Self {
            model_provider_id: model_provider_id.to_string(),
            model: model_info.slug.clone(),
            context_window: model_info.context_window,
            max_context_window: model_info.max_context_window,
            effective_context_window_percent: model_info.effective_context_window_percent,
        }
    }

    fn without_context(model_provider_id: &str, model: String) -> Self {
        Self {
            model_provider_id: model_provider_id.to_string(),
            model,
            context_window: None,
            max_context_window: None,
            effective_context_window_percent: 100,
        }
    }

    fn usable_context_window_tokens(&self, config: &Config) -> Option<i64> {
        let context_window = if let Some(config_context_window) = config.model_context_window {
            self.max_context_window
                .map_or(config_context_window, |max_context_window| {
                    config_context_window.min(max_context_window)
                })
        } else {
            self.context_window.or(self.max_context_window)?
        };
        let usable = context_window.saturating_mul(self.effective_context_window_percent) / 100;
        (usable > 0).then_some(usable)
    }
}

#[derive(Debug, Default)]
pub(crate) struct ModelRouterDiscoveryCache {
    entries: Mutex<BTreeMap<String, ProviderDiscoveryCacheEntry>>,
}

#[derive(Debug, Clone)]
struct ProviderDiscoveryCacheEntry {
    fetched_at: Instant,
    models: Vec<AvailableRouterModel>,
}

impl ModelRouterDiscoveryCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    async fn models_for_provider(
        &self,
        config: &Config,
        provider_id: &str,
        provider: &ModelProviderInfo,
    ) -> Vec<AvailableRouterModel> {
        if let Some(models) = self.cached_models(provider_id).await {
            return models;
        }

        let provider_handle =
            create_model_provider(provider_id, provider.clone(), /*auth_manager*/ None);
        let models_manager = provider_handle.models_manager(
            config.codex_home.to_path_buf(),
            /*config_model_catalog*/ None,
            CollaborationModesConfig::default(),
        );
        let models = models_manager
            .raw_model_catalog(RefreshStrategy::Online)
            .await
            .models
            .iter()
            .map(|model| AvailableRouterModel::from_model_info(provider_id, model))
            .collect();

        let mut entries = self.entries.lock().await;
        entries.insert(
            provider_id.to_string(),
            ProviderDiscoveryCacheEntry {
                fetched_at: Instant::now(),
                models: models.clone(),
            },
        );
        models
    }

    async fn cached_models(&self, provider_id: &str) -> Option<Vec<AvailableRouterModel>> {
        let entries = self.entries.lock().await;
        entries.get(provider_id).and_then(|entry| {
            (entry.fetched_at.elapsed() < ADDITIONAL_PROVIDER_DISCOVERY_TTL)
                .then(|| entry.models.clone())
        })
    }
}

pub(crate) async fn available_router_models(
    config: &Config,
    models_manager: &SharedModelsManager,
    discovery_cache: &ModelRouterDiscoveryCache,
) -> Vec<AvailableRouterModel> {
    let remote_models = models_manager.try_get_remote_models().unwrap_or_else(|err| {
        tracing::debug!(error = %err, "failed to read available model metadata for model router");
        Vec::new()
    });
    let available_presets = models_manager.build_available_models(remote_models.clone());
    available_presets
        .into_iter()
        .map(|preset| {
            remote_models
                .iter()
                .find(|model_info| model_info.slug == preset.model)
                .map(|model_info| {
                    AvailableRouterModel::from_model_info(&config.model_provider_id, model_info)
                })
                .unwrap_or_else(|| {
                    AvailableRouterModel::without_context(&config.model_provider_id, preset.model)
                })
        })
        .chain(additional_provider_router_models(config, discovery_cache).await)
        .collect()
}

async fn additional_provider_router_models(
    config: &Config,
    discovery_cache: &ModelRouterDiscoveryCache,
) -> Vec<AvailableRouterModel> {
    let Some(model_router) = config.model_router.as_ref() else {
        return Vec::new();
    };
    if !model_router.enabled
        || matches!(
            model_router.discovery.unwrap_or_default(),
            ModelRouterDiscoveryToml::Manual
        )
    {
        return Vec::new();
    }

    let mut providers = config.model_providers.iter().collect::<Vec<_>>();
    providers.sort_by_key(|(provider_id, _provider)| *provider_id);
    let mut models = Vec::new();
    for (provider_id, provider) in providers {
        if provider_id == &config.model_provider_id
            || !provider_is_additional_discovery_eligible(provider)
        {
            continue;
        }
        models.extend(
            discovery_cache
                .models_for_provider(config, provider_id, provider)
                .await,
        );
    }
    models
}

fn provider_is_additional_discovery_eligible(provider: &ModelProviderInfo) -> bool {
    provider_has_non_empty_base_url(provider) && provider_has_discovery_auth(provider)
}

fn provider_has_non_empty_base_url(provider: &ModelProviderInfo) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|base_url| !base_url.trim().is_empty())
}

fn provider_has_discovery_auth(provider: &ModelProviderInfo) -> bool {
    if provider
        .experimental_bearer_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty())
    {
        return true;
    }
    if provider.auth.is_some()
        || provider_has_configured_http_headers(provider)
        || provider_has_configured_env_http_headers(provider)
    {
        return true;
    }
    if let Some(aws) = provider.aws.as_ref() {
        return provider_has_aws_discovery_auth(aws);
    }
    if let Some(env_key) = provider.env_key.as_deref() {
        return std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty());
    }
    !provider.requires_openai_auth
}

fn provider_has_configured_http_headers(provider: &ModelProviderInfo) -> bool {
    provider
        .http_headers
        .as_ref()
        .is_some_and(|headers| headers.values().any(|value| !value.trim().is_empty()))
}

fn provider_has_configured_env_http_headers(provider: &ModelProviderInfo) -> bool {
    provider.env_http_headers.as_ref().is_some_and(|headers| {
        headers
            .values()
            .any(|env_key| std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty()))
    })
}

fn provider_has_aws_discovery_auth(aws: &ModelProviderAwsAuthInfo) -> bool {
    aws.profile
        .as_deref()
        .is_some_and(|profile| !profile.trim().is_empty())
        || aws
            .region
            .as_deref()
            .is_some_and(|region| !region.trim().is_empty())
        || env_var_has_non_empty_value("AWS_PROFILE")
        || (env_var_has_non_empty_value("AWS_ACCESS_KEY_ID")
            && env_var_has_non_empty_value("AWS_SECRET_ACCESS_KEY"))
        || (env_var_has_non_empty_value("AWS_BEARER_TOKEN_BEDROCK")
            && (env_var_has_non_empty_value("AWS_REGION")
                || env_var_has_non_empty_value("AWS_DEFAULT_REGION")))
        || (env_var_has_non_empty_value("AWS_WEB_IDENTITY_TOKEN_FILE")
            && env_var_has_non_empty_value("AWS_ROLE_ARN"))
}

fn env_var_has_non_empty_value(env_key: &str) -> bool {
    std::env::var(env_key).is_ok_and(|value| !value.trim().is_empty())
}

struct CandidateSet {
    routes: Vec<CandidateRoute>,
    candidates: Vec<ModelRouterCandidateToml>,
    auto_candidate_count: usize,
    lifecycle_exempt_candidate_count: usize,
}

struct ModelRouterCandidatePool {
    candidates: Vec<ModelRouterCandidateToml>,
    auto_candidate_count: usize,
    lifecycle_exempt_candidate_count: usize,
}

pub(crate) fn model_router_candidate_pool(
    config: &Config,
    available_models: &[AvailableRouterModel],
) -> Result<Vec<ModelRouterCandidateToml>, String> {
    build_model_router_candidate_pool(config, available_models).map(|pool| pool.candidates)
}

pub async fn model_router_candidate_pool_for_config(
    config: &Config,
    models_manager: &SharedModelsManager,
) -> Result<Vec<ModelRouterCandidateToml>, String> {
    let discovery_cache = ModelRouterDiscoveryCache::new();
    let available_models = available_router_models(config, models_manager, &discovery_cache).await;
    model_router_candidate_pool(config, &available_models)
}

fn build_model_router_candidate_pool(
    config: &Config,
    available_models: &[AvailableRouterModel],
) -> Result<ModelRouterCandidatePool, String> {
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(ModelRouterCandidatePool {
            candidates: Vec::new(),
            auto_candidate_count: 0,
            lifecycle_exempt_candidate_count: 0,
        });
    };
    if !model_router.enabled {
        return Ok(ModelRouterCandidatePool {
            candidates: Vec::new(),
            auto_candidate_count: 0,
            lifecycle_exempt_candidate_count: 0,
        });
    }
    let auto_candidates =
        auto_candidates::candidates_from_available_models(config, available_models);
    let available_policy_models = available_models
        .iter()
        .map(|available_model| PolicyAvailableModel {
            provider: available_model.model_provider_id.clone(),
            model: available_model.model.clone(),
        })
        .collect::<Vec<_>>();
    let candidates = policy::candidate_pool_for_discovery(
        model_router,
        &model_router.candidates,
        auto_candidates.clone(),
        &available_policy_models,
        &config.model_provider_id,
    )
    .map_err(|err| err.to_string())?;
    let discovery = model_router.discovery.unwrap_or_default();
    let auto_candidate_count = match discovery {
        ModelRouterDiscoveryToml::Curated => auto_candidates.len(),
        ModelRouterDiscoveryToml::Manual => 0,
        ModelRouterDiscoveryToml::FromRules => candidates.len(),
    };
    let lifecycle_exempt_candidate_count = match discovery {
        ModelRouterDiscoveryToml::Curated => auto_candidates.len(),
        ModelRouterDiscoveryToml::Manual | ModelRouterDiscoveryToml::FromRules => 0,
    };

    Ok(ModelRouterCandidatePool {
        candidates,
        auto_candidate_count,
        lifecycle_exempt_candidate_count,
    })
}

fn build_candidate_set(
    config: &Config,
    task_key: &str,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    overlays: &[ModelRouterMetricOverlay],
) -> Result<CandidateSet, String> {
    if config.model_router.is_none() {
        return Ok(CandidateSet {
            routes: Vec::new(),
            candidates: Vec::new(),
            auto_candidate_count: 0,
            lifecycle_exempt_candidate_count: 0,
        });
    }
    let candidate_pool = build_model_router_candidate_pool(config, available_models)?;
    let candidates = candidate_pool.candidates;
    let auto_candidate_count = candidate_pool.auto_candidate_count;
    let lifecycle_exempt_candidate_count = candidate_pool.lifecycle_exempt_candidate_count;

    let mut routes = Vec::with_capacity(candidates.len() + 1);
    routes.push(CandidateRoute {
        id: Some("incumbent".to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        usable_context_window_tokens: usable_context_window_tokens(
            config,
            &config.model_provider_id,
            config.model.as_deref(),
            available_models,
        ),
        is_incumbent: true,
        metrics: CandidateMetrics::default(),
    });
    let task_class = RouterTaskClass::infer(task_key, prompt_bytes);
    routes.extend(candidates.iter().map(|candidate| {
        let overlay = overlay_for_candidate(candidate, overlays);
        let model_provider = candidate
            .model_provider
            .clone()
            .unwrap_or_else(|| config.model_provider_id.clone());
        CandidateRoute {
            id: candidate.id.clone(),
            model: candidate.model.clone().or_else(|| config.model.clone()),
            model_provider: Some(model_provider.clone()),
            usable_context_window_tokens: usable_context_window_tokens(
                config,
                &model_provider,
                candidate.model.as_deref().or(config.model.as_deref()),
                available_models,
            ),
            is_incumbent: false,
            metrics: candidate_metrics(candidate, task_class, prompt_bytes, overlay),
        }
    }));
    Ok(CandidateSet {
        routes,
        candidates,
        auto_candidate_count,
        lifecycle_exempt_candidate_count,
    })
}

fn usable_context_window_tokens(
    config: &Config,
    model_provider_id: &str,
    model: Option<&str>,
    available_models: &[AvailableRouterModel],
) -> Option<i64> {
    let Some(model) = model else {
        return configured_usable_context_window_tokens(config);
    };
    available_models
        .iter()
        .find(|available_model| {
            available_model.model_provider_id == model_provider_id && available_model.model == model
        })
        .and_then(|available_model| available_model.usable_context_window_tokens(config))
        .or_else(|| configured_usable_context_window_tokens(config))
}

fn configured_usable_context_window_tokens(config: &Config) -> Option<i64> {
    config
        .model_context_window
        .map(|context_window| context_window.saturating_mul(95) / 100)
        .filter(|context_window| *context_window > 0)
}

fn candidate_metrics(
    candidate: &ModelRouterCandidateToml,
    task_class: RouterTaskClass,
    prompt_bytes: usize,
    overlay: Option<&ModelRouterMetricOverlay>,
) -> CandidateMetrics {
    let explicit_estimated_cost_usd_micros = token_price_from_candidate(candidate).map(|price| {
        estimate_token_cost(
            &estimate_task_usage(prompt_bytes, task_class),
            &price,
            /*confidence*/ 1.0,
        )
        .usd_micros
    });
    CandidateMetrics {
        intelligence_score: candidate
            .intelligence_score
            .or_else(|| overlay.and_then(|overlay| overlay.intelligence_score)),
        success_rate: candidate
            .success_rate
            .or_else(|| overlay.and_then(|overlay| overlay.success_rate)),
        median_latency_ms: candidate
            .median_latency_ms
            .or_else(|| overlay.and_then(|overlay| overlay.median_latency_ms)),
        estimated_cost_usd_micros: explicit_estimated_cost_usd_micros
            .or_else(|| overlay.and_then(|overlay| overlay.estimated_cost_usd_micros)),
    }
}

fn build_model_router_accounting(
    config: &Config,
    task_key: &str,
    selected_index: usize,
    selected_route: &failover::ModelRouterRouteKey,
    candidate_set: &CandidateSet,
) -> ModelRouterAccounting {
    let actual_price = if selected_index == 0 {
        incumbent_token_price(config, &candidate_set.candidates)
    } else {
        selected_index
            .checked_sub(1)
            .and_then(|candidate_index| candidate_set.candidates.get(candidate_index))
            .and_then(token_price_from_candidate)
    };
    let counterfactual_price = if selected_index == 0 {
        actual_price
    } else {
        incumbent_token_price(config, &candidate_set.candidates)
    };
    ModelRouterAccounting {
        task_key: task_key.to_string(),
        model_provider: selected_route.model_provider.clone(),
        model: selected_route.model.clone(),
        account_id: selected_route
            .account
            .clone()
            .or_else(|| selected_route.account_pool.clone()),
        actual_price,
        actual_price_confidence: price_confidence(actual_price),
        counterfactual_price,
        counterfactual_price_confidence: price_confidence(counterfactual_price),
    }
}

fn incumbent_token_price(
    config: &Config,
    candidates: &[ModelRouterCandidateToml],
) -> Option<TokenPrice> {
    candidates
        .iter()
        .find(|candidate| {
            let provider_matches = candidate
                .model_provider
                .as_deref()
                .is_none_or(|provider| provider == config.model_provider_id);
            let model_matches = candidate.model.as_deref() == config.model.as_deref();
            provider_matches && model_matches && token_price_from_candidate(candidate).is_some()
        })
        .and_then(token_price_from_candidate)
}

fn price_confidence(price: Option<TokenPrice>) -> f64 {
    if price.is_some() { 1.0 } else { 0.0 }
}

fn cost_estimate_for_price(
    token_usage: &TokenUsage,
    price: Option<&TokenPrice>,
    confidence: f64,
) -> CostEstimate {
    price
        .map(|price| estimate_token_cost(token_usage, price, confidence))
        .unwrap_or_else(|| CostEstimate::zero_with_confidence(/*confidence*/ 0.0))
}

fn overlay_for_candidate<'a>(
    candidate: &ModelRouterCandidateToml,
    overlays: &'a [ModelRouterMetricOverlay],
) -> Option<&'a ModelRouterMetricOverlay> {
    let identity = model_router_candidate_identity_key(candidate);
    overlays
        .iter()
        .find(|overlay| overlay.candidate_identity == identity)
}

pub(crate) fn model_router_candidate_identity(
    candidate: &ModelRouterCandidateToml,
) -> ModelRouterCandidateIdentity {
    ModelRouterCandidateIdentity {
        id: candidate.id.clone(),
        model: candidate.model.clone(),
        model_provider: candidate.model_provider.clone(),
        service_tier: candidate.service_tier.map(|value| format!("{value:?}")),
        reasoning_effort: candidate.reasoning_effort.map(|value| format!("{value:?}")),
        account_pool: candidate.account_pool.clone(),
        account: candidate.account.clone(),
    }
}

pub(crate) fn model_router_candidate_identity_key(candidate: &ModelRouterCandidateToml) -> String {
    serde_json::to_string(&model_router_candidate_identity(candidate))
        .unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn token_price_from_candidate(
    candidate: &ModelRouterCandidateToml,
) -> Option<TokenPrice> {
    let has_price = candidate.input_price_per_million.is_some()
        || candidate.cached_input_price_per_million.is_some()
        || candidate.output_price_per_million.is_some()
        || candidate.reasoning_output_price_per_million.is_some();
    if !has_price {
        return None;
    }
    let input_per_million = candidate.input_price_per_million.unwrap_or(0.0);
    Some(TokenPrice {
        input_per_million,
        cached_input_per_million: candidate
            .cached_input_price_per_million
            .unwrap_or(input_per_million),
        output_per_million: candidate
            .output_price_per_million
            .unwrap_or(input_per_million),
        reasoning_output_per_million: candidate.reasoning_output_price_per_million,
    })
}

pub(crate) fn apply_candidate(
    config: &mut Config,
    candidate: &ModelRouterCandidateToml,
) -> Result<(), String> {
    if let Some(model_provider_id) = &candidate.model_provider {
        let model_provider = config
            .model_providers
            .get(model_provider_id)
            .ok_or_else(|| {
                format!(
                    "model_router candidate references unknown model_provider `{model_provider_id}`"
                )
            })?
            .clone();
        config.model_provider_id = model_provider_id.clone();
        config.model_provider = model_provider;
    }
    if let Some(model) = &candidate.model {
        config.model = Some(model.clone());
    }
    if let Some(service_tier) = candidate.service_tier {
        config.service_tier = Some(service_tier);
    }
    if let Some(reasoning_effort) = candidate
        .reasoning_effort
        .and_then(codex_config::config_toml::ModelRouterReasoningEffortToml::as_reasoning_effort)
    {
        config.model_reasoning_effort = Some(reasoning_effort);
    }
    if let Some(account_pool) = &candidate.account_pool {
        set_account_pool(config, account_pool)?;
    }
    if let Some(account) = &candidate.account {
        set_single_account(config, account);
    }
    Ok(())
}

fn set_account_pool(config: &mut Config, account_pool: &str) -> Result<(), String> {
    let configured = config
        .account_pool
        .as_mut()
        .ok_or_else(|| format!("model_router candidate references account_pool `{account_pool}`, but [account_pool] is not configured"))?;
    if !configured.pools.contains_key(account_pool) {
        return Err(format!(
            "model_router candidate references unknown account_pool `{account_pool}`"
        ));
    }
    configured.enabled = true;
    configured.default_pool = Some(account_pool.to_string());
    Ok(())
}

fn set_single_account(config: &mut Config, account: &str) {
    let pool_id = format!("account:{account}");
    let account_pool = config.account_pool.get_or_insert_with(|| AccountPoolToml {
        enabled: true,
        default_pool: None,
        pools: BTreeMap::new(),
    });
    account_pool.enabled = true;
    account_pool.default_pool = Some(pool_id.clone());
    account_pool.pools.insert(
        pool_id,
        AccountPoolDefinitionToml {
            provider: OPENAI_PROVIDER_ID.to_string(),
            policy: AccountPoolPolicyToml::Drain,
            accounts: vec![account.to_string()],
        },
    );
}

#[cfg(test)]
mod tests {
    use codex_config::config_toml::ModelRouterBiasRuleToml;
    use codex_config::config_toml::ModelRouterBiasToml;
    use codex_config::config_toml::ModelRouterCandidateToml;
    use codex_config::config_toml::ModelRouterDiscoveryToml;
    use codex_config::config_toml::ModelRouterLifecycleDefaultsToml;
    use codex_config::config_toml::ModelRouterLifecycleToml;
    use codex_config::config_toml::ModelRouterModelRuleToml;
    use codex_config::config_toml::ModelRouterModelRuleTypeToml;
    use codex_config::config_toml::ModelRouterModelSelectorToml;
    use codex_config::config_toml::ModelRouterModelsToml;
    use codex_config::config_toml::ModelRouterReasoningEffortToml;
    use codex_config::config_toml::ModelRouterToml;
    use codex_login::AuthManager;
    use codex_model_provider_info::DEEPSEEK_PROVIDER_ID;
    use codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID;
    use codex_model_provider_info::OPENAI_PROVIDER_ID;
    use codex_model_router::RouterSavings;
    use codex_models_manager::manager::SharedModelsManager;
    use codex_models_manager::manager::StaticModelsManager;
    use codex_models_manager::model_info::model_info_from_slug;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::ServiceTier;
    use codex_protocol::openai_models::ModelsResponse;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_state::ModelRouterLifecyclePromotionRecord;
    use codex_state::ModelRouterMetricOverlay;
    use codex_state::ModelRouterShadowEvaluationRecord;
    use codex_state::ModelRouterUsageGroupBy;
    use codex_state::ModelRouterUsageQuery;
    use codex_state::StateRuntime;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tempfile::TempDir;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::config;

    fn retain_only_additional_providers(config: &mut Config, additional_provider_ids: &[&str]) {
        let active_provider_id = config.model_provider_id.clone();
        config.model_providers.retain(|provider_id, _provider| {
            provider_id == &active_provider_id
                || additional_provider_ids.contains(&provider_id.as_str())
        });
    }

    #[test]
    fn chat_source_task_key_tracks_collaboration_mode() {
        assert_eq!(
            ModelRouterSource::Chat(ModeKind::Default).task_key(),
            "chat.default"
        );
        assert_eq!(
            ModelRouterSource::Chat(ModeKind::Plan).task_key(),
            "chat.plan"
        );
        assert_eq!(
            ModelRouterSource::Chat(ModeKind::Codex).task_key(),
            "chat.codex"
        );
        assert_eq!(
            ModelRouterSource::Chat(ModeKind::CodexConfigEdit).task_key(),
            "chat.codex"
        );
        assert_eq!(
            ModelRouterSource::Chat(ModeKind::Workflow).task_key(),
            "chat.workflow"
        );
    }

    #[tokio::test]
    async fn no_available_models_leaves_incumbent_unchanged() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });

        apply_model_router(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            80,
            &[],
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("parent-model"));
    }

    #[tokio::test]
    async fn no_explicit_candidates_uses_available_models() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let available_models = vec![available_model("gpt-5.3-codex-spark")];

        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
    }

    #[tokio::test]
    async fn available_router_models_discovers_custom_registered_provider() {
        let server = MockServer::start().await;
        mount_models_response(&server, 200, vec!["deepseek-chat"]).await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        config.model_providers.insert(
            "deepseek-custom".to_string(),
            custom_provider_for_base_url(server.uri()),
        );
        retain_only_additional_providers(&mut config, &["deepseek-custom"]);

        let available_models = available_router_models(
            &config,
            &empty_models_manager(),
            &ModelRouterDiscoveryCache::new(),
        )
        .await;
        let candidate_set =
            build_candidate_set(&config, "module.repo_ci.triage", 80, &available_models, &[])
                .expect("candidate set should build");

        assert_eq!(
            candidate_set.candidates,
            vec![ModelRouterCandidateToml {
                id: Some("auto:deepseek-custom:deepseek-chat".to_string()),
                model: Some("deepseek-chat".to_string()),
                model_provider: Some("deepseek-custom".to_string()),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn available_router_models_discovers_ready_builtin_deepseek_provider() {
        let server = MockServer::start().await;
        mount_models_response(&server, 200, vec!["deepseek-v4-flash"]).await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let deepseek = config
            .model_providers
            .get_mut(DEEPSEEK_PROVIDER_ID)
            .expect("DeepSeek provider should be built in");
        deepseek.base_url = Some(server.uri());
        deepseek.env_key = None;
        deepseek.experimental_bearer_token = Some("deepseek-token".to_string());
        retain_only_additional_providers(&mut config, &[DEEPSEEK_PROVIDER_ID]);

        let available_models = available_router_models(
            &config,
            &empty_models_manager(),
            &ModelRouterDiscoveryCache::new(),
        )
        .await;
        let candidate_set =
            build_candidate_set(&config, "module.repo_ci.triage", 80, &available_models, &[])
                .expect("candidate set should build");

        assert_eq!(
            candidate_set.candidates,
            vec![ModelRouterCandidateToml {
                id: Some("auto:deepseek:deepseek-v4-flash".to_string()),
                model: Some("deepseek-v4-flash".to_string()),
                model_provider: Some(DEEPSEEK_PROVIDER_ID.to_string()),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn available_router_models_discovers_ready_builtin_no_auth_provider() {
        let server = MockServer::start().await;
        mount_models_response(&server, 200, vec!["llama3.2"]).await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let ollama = config
            .model_providers
            .get_mut(OLLAMA_OSS_PROVIDER_ID)
            .expect("Ollama provider should be built in");
        ollama.base_url = Some(server.uri());
        retain_only_additional_providers(&mut config, &[OLLAMA_OSS_PROVIDER_ID]);

        let available_models = available_router_models(
            &config,
            &empty_models_manager(),
            &ModelRouterDiscoveryCache::new(),
        )
        .await;
        let candidate_set =
            build_candidate_set(&config, "module.repo_ci.triage", 80, &available_models, &[])
                .expect("candidate set should build");

        assert_eq!(
            candidate_set.candidates,
            vec![ModelRouterCandidateToml {
                id: Some("auto:ollama:llama3.2".to_string()),
                model: Some("llama3.2".to_string()),
                model_provider: Some(OLLAMA_OSS_PROVIDER_ID.to_string()),
                ..Default::default()
            }]
        );
    }

    #[tokio::test]
    async fn ready_builtin_deepseek_candidate_is_production_selectable_by_default() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let available_models = vec![available_model_for_provider(
            DEEPSEEK_PROVIDER_ID,
            "deepseek-v4-flash",
            Some(64_000),
            Some(64_000),
        )];

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model_provider_id, DEEPSEEK_PROVIDER_ID);
        assert_eq!(config.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[tokio::test]
    async fn custom_discovered_candidate_switches_provider_and_model() {
        let server = MockServer::start().await;
        mount_models_response(&server, 200, vec!["deepseek-chat"]).await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        config.model_providers.insert(
            "deepseek-custom".to_string(),
            custom_provider_for_base_url(server.uri()),
        );
        retain_only_additional_providers(&mut config, &["deepseek-custom"]);

        let available_models = available_router_models(
            &config,
            &empty_models_manager(),
            &ModelRouterDiscoveryCache::new(),
        )
        .await;
        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model_provider_id, "deepseek-custom");
        assert_eq!(config.model.as_deref(), Some("deepseek-chat"));
    }

    #[tokio::test]
    async fn custom_provider_discovery_failure_falls_back_without_error() {
        let server = MockServer::start().await;
        mount_models_response(&server, 500, Vec::new()).await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        config.model_providers.insert(
            "deepseek-custom".to_string(),
            custom_provider_for_base_url(server.uri()),
        );
        retain_only_additional_providers(&mut config, &["deepseek-custom"]);

        let available_models = available_router_models(
            &config,
            &empty_models_manager(),
            &ModelRouterDiscoveryCache::new(),
        )
        .await;
        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
        )
        .expect("router should fall back");

        assert_eq!(config.model_provider_id, "openai");
        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn manual_discovery_ignores_available_models() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            discovery: Some(ModelRouterDiscoveryToml::Manual),
            candidates: Vec::new(),
            ..Default::default()
        });
        let available_models = vec![available_model("gpt-5.3-codex-spark")];

        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn from_rules_discovery_expands_available_models() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            discovery: Some(ModelRouterDiscoveryToml::FromRules),
            bias: Some(ModelRouterBiasToml {
                rules: vec![ModelRouterBiasRuleToml {
                    id: Some("spark".to_string()),
                    tasks: vec!["module.repo_ci.triage".to_string()],
                    except_tasks: Vec::new(),
                    models: vec![ModelRouterModelSelectorToml {
                        provider: Some("openai".to_string()),
                        model: Some("/spark/".to_string()),
                    }],
                    score_bias: 0.10,
                }],
            }),
            ..Default::default()
        });
        let available_models = vec![available_model("gpt-5.3-codex-spark")];

        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
    }

    #[tokio::test]
    async fn hard_policy_require_and_exclude_apply_before_scoring() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.3-codex-spark".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![
                ModelRouterCandidateToml {
                    id: Some("spark".to_string()),
                    model: Some("gpt-5.3-codex-spark".to_string()),
                    ..Default::default()
                },
                ModelRouterCandidateToml {
                    id: Some("top".to_string()),
                    model: Some("gpt-5.5".to_string()),
                    ..Default::default()
                },
            ],
            models: Some(ModelRouterModelsToml {
                rules: vec![
                    ModelRouterModelRuleToml {
                        id: Some("review-top".to_string()),
                        rule_type: ModelRouterModelRuleTypeToml::Require,
                        tasks: vec!["/review$/".to_string()],
                        except_tasks: Vec::new(),
                        models: vec![ModelRouterModelSelectorToml {
                            provider: Some("openai".to_string()),
                            model: Some("/^gpt-5\\.5/".to_string()),
                        }],
                    },
                    ModelRouterModelRuleToml {
                        id: Some("no-spark".to_string()),
                        rule_type: ModelRouterModelRuleTypeToml::Exclude,
                        tasks: vec!["/review$/".to_string()],
                        except_tasks: Vec::new(),
                        models: vec![ModelRouterModelSelectorToml {
                            provider: Some("openai".to_string()),
                            model: Some("/spark/".to_string()),
                        }],
                    },
                ],
            }),
            ..Default::default()
        });

        apply_model_router(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            80,
            &[],
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.5"));
    }

    #[tokio::test]
    async fn hard_policy_errors_when_no_routes_remain() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            models: Some(ModelRouterModelsToml {
                rules: vec![ModelRouterModelRuleToml {
                    id: Some("review-missing".to_string()),
                    rule_type: ModelRouterModelRuleTypeToml::Require,
                    tasks: vec!["/review$/".to_string()],
                    except_tasks: Vec::new(),
                    models: vec![ModelRouterModelSelectorToml {
                        provider: Some("openai".to_string()),
                        model: Some("missing-model".to_string()),
                    }],
                }],
            }),
            ..Default::default()
        });

        let err = apply_model_router(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            80,
            &[],
        )
        .expect_err("hard policy should reject all routes");

        assert!(err.contains("left no eligible routes"));
    }

    #[tokio::test]
    async fn latency_sensitive_task_applies_fast_candidate() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                model: Some("gpt-5.3-codex-spark".to_string()),
                service_tier: Some(ServiceTier::Flex),
                reasoning_effort: Some(ModelRouterReasoningEffortToml::Low),
                account: Some("spark-account".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(
            config
                .account_pool
                .as_ref()
                .and_then(|pool| pool.default_pool.as_deref()),
            Some("account:spark-account")
        );
    }

    #[tokio::test]
    async fn routed_request_usage_records_production_ledger_row() {
        let (codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(no_shadow_lifecycle()),
            candidates: vec![
                ModelRouterCandidateToml {
                    id: Some("incumbent-price".to_string()),
                    model: Some("gpt-5.4".to_string()),
                    input_price_per_million: Some(5.0),
                    output_price_per_million: Some(10.0),
                    ..Default::default()
                },
                ModelRouterCandidateToml {
                    id: Some("fast".to_string()),
                    model: Some("gpt-5.3-codex-spark".to_string()),
                    input_price_per_million: Some(1.0),
                    output_price_per_million: Some(2.0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");
        record_model_router_request_usage(
            Some(runtime.as_ref()),
            config.model_router_accounting.as_ref(),
            &TokenUsage {
                input_tokens: 1_000_000,
                cached_input_tokens: 0,
                output_tokens: 1_000_000,
                reasoning_output_tokens: 0,
                total_tokens: 2_000_000,
            },
            "completed",
        )
        .await;

        let summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms: Utc::now().timestamp_millis(),
                task_key: Some("module.repo_ci.triage".to_string()),
                group_by: ModelRouterUsageGroupBy::Task,
            })
            .await
            .expect("usage summary");

        drop(codex_home);
        assert_eq!(summary.totals.request_count, 1);
        assert_eq!(
            summary.totals.savings,
            RouterSavings {
                actual_production_cost_usd_micros: 3_000_000,
                router_overhead_cost_usd_micros: 0,
                counterfactual_cost_usd_micros: 15_000_000,
                gross_savings_usd_micros: 12_000_000,
                net_savings_usd_micros: 12_000_000,
            }
        );
        assert_eq!(summary.totals.average_price_confidence, 1.0);
    }

    #[tokio::test]
    async fn routed_request_usage_records_one_row_per_terminal_outcome() {
        let (_codex_home, runtime) = state_runtime().await;
        let accounting = ModelRouterAccounting {
            task_key: "module.repo_ci.triage".to_string(),
            model_provider: "openai".to_string(),
            model: Some("gpt-5.3-codex-spark".to_string()),
            account_id: None,
            actual_price: Some(TokenPrice {
                input_per_million: 1.0,
                cached_input_per_million: 1.0,
                output_per_million: 2.0,
                reasoning_output_per_million: None,
            }),
            actual_price_confidence: 1.0,
            counterfactual_price: None,
            counterfactual_price_confidence: 0.0,
        };

        record_model_router_request_usage(
            Some(runtime.as_ref()),
            Some(&accounting),
            &TokenUsage {
                input_tokens: 1_000_000,
                cached_input_tokens: 0,
                output_tokens: 1_000_000,
                reasoning_output_tokens: 0,
                total_tokens: 2_000_000,
            },
            "completed",
        )
        .await;
        for outcome in ["stream_error", "aborted", "error"] {
            record_model_router_request_usage(
                Some(runtime.as_ref()),
                Some(&accounting),
                &TokenUsage::default(),
                outcome,
            )
            .await;
        }

        let summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms: Utc::now().timestamp_millis(),
                task_key: Some("module.repo_ci.triage".to_string()),
                group_by: ModelRouterUsageGroupBy::RequestKind,
            })
            .await
            .expect("usage summary");

        assert_eq!(
            summary.totals,
            codex_state::ModelRouterUsageTotals {
                request_count: 4,
                production_request_count: 4,
                overhead_request_count: 0,
                token_usage: TokenUsage {
                    input_tokens: 1_000_000,
                    cached_input_tokens: 0,
                    output_tokens: 1_000_000,
                    reasoning_output_tokens: 0,
                    total_tokens: 2_000_000,
                },
                savings: RouterSavings {
                    actual_production_cost_usd_micros: 3_000_000,
                    router_overhead_cost_usd_micros: 0,
                    counterfactual_cost_usd_micros: 0,
                    gross_savings_usd_micros: -3_000_000,
                    net_savings_usd_micros: -3_000_000,
                },
                average_price_confidence: 0.0,
                minimum_price_confidence: 0.0,
                coverage: codex_state::ModelRouterUsageCoverage {
                    missing_price_rows: 4,
                    low_confidence_price_rows: 0,
                    zero_token_rows: 3,
                    production_rows_missing_actual_cost: 0,
                    production_rows_missing_counterfactual: 1,
                },
            }
        );
    }

    #[tokio::test]
    async fn direct_internal_ai_sources_record_production_ledger_rows() {
        let (_codex_home, runtime) = state_runtime().await;
        let sources = [
            ModelRouterSource::SubAgent(SubAgentSource::Compact),
            ModelRouterSource::Module("memories.extract"),
            ModelRouterSource::Module("repo_ci.triage"),
        ];

        for source in sources {
            let mut config = config::test_config().await;
            config.model = Some("gpt-5.4".to_string());
            config.model_router = Some(ModelRouterToml {
                enabled: true,
                lifecycle: Some(no_shadow_lifecycle()),
                candidates: vec![
                    ModelRouterCandidateToml {
                        id: Some("incumbent-price".to_string()),
                        model: Some("gpt-5.4".to_string()),
                        input_price_per_million: Some(5.0),
                        output_price_per_million: Some(10.0),
                        ..Default::default()
                    },
                    ModelRouterCandidateToml {
                        id: Some("routed".to_string()),
                        model: Some("gpt-5.3-codex-spark".to_string()),
                        input_price_per_million: Some(1.0),
                        output_price_per_million: Some(2.0),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            });
            apply_model_router_with_state(&mut config, source, 80, &[], Some(runtime.as_ref()))
                .await
                .expect("router should apply");
            record_model_router_request_usage_for_config(
                Some(runtime.as_ref()),
                &config,
                &TokenUsage {
                    input_tokens: 100,
                    cached_input_tokens: 0,
                    output_tokens: 20,
                    reasoning_output_tokens: 0,
                    total_tokens: 120,
                },
                "completed",
            )
            .await;
        }

        let summary = runtime
            .model_router_usage_summary(ModelRouterUsageQuery {
                window_start_ms: None,
                window_end_ms: Utc::now().timestamp_millis(),
                task_key: None,
                group_by: ModelRouterUsageGroupBy::Task,
            })
            .await
            .expect("usage summary");
        let mut keys = summary
            .groups
            .iter()
            .map(|group| group.key.as_str())
            .collect::<Vec<_>>();
        keys.sort_unstable();

        assert_eq!(
            keys,
            vec![
                "module.memories.extract",
                "module.repo_ci.triage",
                "subagent.compact"
            ]
        );
        assert_eq!(summary.totals.request_count, 3);
        assert_eq!(summary.totals.production_request_count, 3);
    }

    #[tokio::test]
    async fn auth_manager_for_config_rebuilds_for_routed_account_pool() {
        let mut config = config::test_config().await;
        config.account_pool = Some(AccountPoolToml {
            enabled: true,
            default_pool: Some("base".to_string()),
            pools: BTreeMap::from([(
                "base".to_string(),
                AccountPoolDefinitionToml {
                    provider: OPENAI_PROVIDER_ID.to_string(),
                    policy: AccountPoolPolicyToml::Drain,
                    accounts: vec!["base-account".to_string()],
                },
            )]),
        });
        let parent =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false);
        let mut routed_config = config;

        set_single_account(&mut routed_config, "work-account");
        let routed = auth_manager_for_config(&routed_config, &parent);

        assert_eq!(parent.default_account_pool_id().as_deref(), Some("base"));
        assert_eq!(
            routed.default_account_pool_id().as_deref(),
            Some("account:work-account")
        );
    }

    #[tokio::test]
    async fn quality_sensitive_task_applies_best_quality_candidate() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.3-codex-spark".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![
                ModelRouterCandidateToml {
                    id: Some("small".to_string()),
                    model: Some("gpt-5.3-codex-spark".to_string()),
                    ..Default::default()
                },
                ModelRouterCandidateToml {
                    id: Some("large".to_string()),
                    model: Some("gpt-5.5".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        apply_model_router(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            80,
            &[],
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.5"));
    }

    #[tokio::test]
    async fn skips_available_model_when_prompt_exceeds_context_window() {
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let available_models = vec![available_model_with_context(
            "gpt-5.3-codex-spark",
            Some(1_000),
            Some(1_000),
        )];

        apply_model_router(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            8_000,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn leaves_config_unchanged_when_candidate_fails() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("broken".to_string()),
                model: Some("gpt-5.5".to_string()),
                model_provider: Some("missing-provider".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        let err = apply_model_router(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            1,
            &[],
        )
        .expect_err("unknown provider should fail");

        assert!(err.contains("unknown model_provider"));
        assert_eq!(config.model.as_deref(), Some("parent-model"));
    }

    #[tokio::test]
    async fn explicit_toml_metrics_beat_applied_overlays() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            intelligence_score: Some(0.1),
            success_rate: Some(1.0),
            median_latency_ms: Some(1_000),
            ..Default::default()
        };
        runtime
            .upsert_model_router_metric_overlay(ModelRouterMetricOverlay {
                candidate_identity: model_router_candidate_identity_key(&candidate),
                intelligence_score: Some(0.99),
                success_rate: Some(1.0),
                median_latency_ms: Some(1_000),
                estimated_cost_usd_micros: None,
                source_report_id: "report".to_string(),
                config_fingerprint: "fingerprint".to_string(),
            })
            .await
            .expect("upsert overlay");
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(no_shadow_lifecycle()),
            candidates: vec![candidate],
            ..Default::default()
        });

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.review"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn applied_overlays_beat_inferred_defaults() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("unknown-fast".to_string()),
            model: Some("local-fast".to_string()),
            ..Default::default()
        };
        runtime
            .upsert_model_router_metric_overlay(ModelRouterMetricOverlay {
                candidate_identity: model_router_candidate_identity_key(&candidate),
                intelligence_score: Some(0.95),
                success_rate: Some(0.99),
                median_latency_ms: Some(1_000),
                estimated_cost_usd_micros: Some(1),
                source_report_id: "report".to_string(),
                config_fingerprint: "fingerprint".to_string(),
            })
            .await
            .expect("upsert overlay");
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(no_shadow_lifecycle()),
            candidates: vec![candidate],
            ..Default::default()
        });

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("local-fast"));
    }

    #[tokio::test]
    async fn promoted_lifecycle_candidate_wins_when_still_eligible() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let promoted_candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            ..Default::default()
        };
        let promoted_identity = model_router_candidate_identity_key(&promoted_candidate);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![
                promoted_candidate,
                ModelRouterCandidateToml {
                    id: Some("top".to_string()),
                    model: Some("gpt-5.5".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        runtime
            .upsert_model_router_lifecycle_promotion(ModelRouterLifecyclePromotionRecord {
                task_key: "subagent.review".to_string(),
                candidate_identity: promoted_identity,
                base_candidate_identity: "base".to_string(),
                status: "promoted".to_string(),
                rule_id: Some("review".to_string()),
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.3-codex-spark".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                promoted_at_ms: 1,
                updated_at_ms: 1,
                reason: None,
            })
            .await
            .expect("upsert promotion");

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::SubAgent(SubAgentSource::Review),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
    }

    #[tokio::test]
    async fn lifecycle_candidate_shadows_until_promoted() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_defaults()),
            candidates: vec![ModelRouterCandidateToml {
                id: Some("spark".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                intelligence_score: Some(0.95),
                success_rate: Some(1.0),
                median_latency_ms: Some(1_000),
                ..Default::default()
            }],
            ..Default::default()
        });

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn default_lifecycle_candidate_shadows_until_promoted() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("spark".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                intelligence_score: Some(0.95),
                success_rate: Some(1.0),
                median_latency_ms: Some(1_000),
                ..Default::default()
            }],
            ..Default::default()
        });

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
    }

    #[tokio::test]
    async fn lifecycle_promotes_candidate_after_shadow_gates_pass() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            intelligence_score: Some(0.95),
            success_rate: Some(1.0),
            median_latency_ms: Some(1_000),
            ..Default::default()
        };
        let candidate_identity = model_router_candidate_identity_key(&candidate);
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_defaults()),
            candidates: vec![candidate],
            ..Default::default()
        });
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_PROMOTION,
            &candidate_identity,
            &base_identity,
            true,
            1,
        )
        .await;
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_PROMOTION,
            &candidate_identity,
            &base_identity,
            true,
            2,
        )
        .await;

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some("module.repo_ci.triage"))
                .await
                .expect("promotions")
                .first()
                .map(|promotion| promotion.status.as_str()),
            Some(LIFECYCLE_STATUS_PROMOTED)
        );
        let events = lifecycle_events(runtime.as_ref(), "module.repo_ci.triage").await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            codex_state::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTED
        );
        assert_eq!(
            events[0].source,
            codex_state::MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO
        );
        assert_eq!(events[0].shadow_phase.as_deref(), Some("promotion"));
        assert_eq!(events[0].shadow_evaluated_count, Some(2));
    }

    #[tokio::test]
    async fn lifecycle_demotes_candidate_after_monitoring_gates_fail() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            intelligence_score: Some(0.95),
            success_rate: Some(1.0),
            median_latency_ms: Some(1_000),
            ..Default::default()
        };
        let candidate_identity = model_router_candidate_identity_key(&candidate);
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_defaults()),
            candidates: vec![candidate],
            ..Default::default()
        });
        runtime
            .upsert_model_router_lifecycle_promotion(ModelRouterLifecyclePromotionRecord {
                task_key: "module.repo_ci.triage".to_string(),
                candidate_identity: candidate_identity.clone(),
                base_candidate_identity: base_identity.clone(),
                status: LIFECYCLE_STATUS_PROMOTED.to_string(),
                rule_id: None,
                production_model_provider: Some("openai".to_string()),
                production_model: Some("gpt-5.3-codex-spark".to_string()),
                base_model_provider: Some("openai".to_string()),
                base_model: Some("gpt-5.4".to_string()),
                promoted_at_ms: 1,
                updated_at_ms: 1,
                reason: None,
            })
            .await
            .expect("upsert promotion");
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_MONITORING,
            &candidate_identity,
            &base_identity,
            false,
            1,
        )
        .await;
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_MONITORING,
            &candidate_identity,
            &base_identity,
            false,
            2,
        )
        .await;

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some("module.repo_ci.triage"))
                .await
                .expect("promotions")
                .first()
                .map(|promotion| promotion.status.as_str()),
            Some("demoted")
        );
        let events = lifecycle_events(runtime.as_ref(), "module.repo_ci.triage").await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            codex_state::MODEL_ROUTER_LIFECYCLE_EVENT_DEMOTED
        );
        assert_eq!(events[0].shadow_phase.as_deref(), Some("monitoring"));
        assert!(
            events[0]
                .failed_gates_json
                .as_deref()
                .is_some_and(|json| json.contains("min_success_rate"))
        );
    }

    #[tokio::test]
    async fn lifecycle_records_blocked_promotion_after_failed_gates() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            intelligence_score: Some(0.95),
            success_rate: Some(1.0),
            median_latency_ms: Some(1_000),
            ..Default::default()
        };
        let candidate_identity = model_router_candidate_identity_key(&candidate);
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_defaults()),
            candidates: vec![candidate],
            ..Default::default()
        });
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_PROMOTION,
            &candidate_identity,
            &base_identity,
            false,
            1,
        )
        .await;
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_PROMOTION,
            &candidate_identity,
            &base_identity,
            false,
            2,
        )
        .await;

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");
        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply again");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
        let events = lifecycle_events(runtime.as_ref(), "module.repo_ci.triage").await;
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            codex_state::MODEL_ROUTER_LIFECYCLE_EVENT_PROMOTION_BLOCKED
        );
        assert_eq!(events[0].shadow_phase.as_deref(), Some("promotion"));
        assert_eq!(events[0].shadow_latest_evaluation_id, Some(2));
        assert!(
            events[0]
                .failed_gates_json
                .as_deref()
                .is_some_and(|json| json.contains("min_success_rate"))
        );
    }

    #[tokio::test]
    async fn lifecycle_does_not_record_blocked_promotion_without_enough_samples() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = config::test_config().await;
        config.model = Some("gpt-5.4".to_string());
        let candidate = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            intelligence_score: Some(0.95),
            success_rate: Some(1.0),
            median_latency_ms: Some(1_000),
            ..Default::default()
        };
        let candidate_identity = model_router_candidate_identity_key(&candidate);
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_defaults()),
            candidates: vec![candidate],
            ..Default::default()
        });
        record_shadow(
            runtime.as_ref(),
            "module.repo_ci.triage",
            LIFECYCLE_PHASE_PROMOTION,
            &candidate_identity,
            &base_identity,
            false,
            1,
        )
        .await;

        apply_model_router_with_state(
            &mut config,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            Some(runtime.as_ref()),
        )
        .await
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.4"));
        assert!(
            lifecycle_events(runtime.as_ref(), "module.repo_ci.triage")
                .await
                .is_empty()
        );
    }

    fn available_model(model: &str) -> AvailableRouterModel {
        available_model_with_context(model, Some(272_000), Some(272_000))
    }

    fn available_model_with_context(
        model: &str,
        context_window: Option<i64>,
        max_context_window: Option<i64>,
    ) -> AvailableRouterModel {
        available_model_for_provider("openai", model, context_window, max_context_window)
    }

    fn available_model_for_provider(
        model_provider_id: &str,
        model: &str,
        context_window: Option<i64>,
        max_context_window: Option<i64>,
    ) -> AvailableRouterModel {
        AvailableRouterModel {
            model_provider_id: model_provider_id.to_string(),
            model: model.to_string(),
            context_window,
            max_context_window,
            effective_context_window_percent: 95,
        }
    }

    fn empty_models_manager() -> SharedModelsManager {
        Arc::new(StaticModelsManager::new(
            /*auth_manager*/ None,
            ModelsResponse { models: Vec::new() },
            Default::default(),
        ))
    }

    fn custom_provider_for_base_url(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            name: "DeepSeek".to_string(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    async fn mount_models_response(server: &MockServer, status: u16, models: Vec<&str>) {
        let models = models.into_iter().map(model_info_from_slug).collect();
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(status).set_body_json(ModelsResponse { models }))
            .expect(1)
            .mount(server)
            .await;
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }

    fn lifecycle_defaults() -> ModelRouterLifecycleToml {
        ModelRouterLifecycleToml {
            defaults: Some(ModelRouterLifecycleDefaultsToml {
                window: Some("all".to_string()),
                min_evaluated: Some(2),
                min_confidence: Some(0.9),
                min_success_rate: Some(0.9),
                ..Default::default()
            }),
            rules: Vec::new(),
        }
    }

    fn no_shadow_lifecycle() -> ModelRouterLifecycleToml {
        ModelRouterLifecycleToml {
            defaults: Some(ModelRouterLifecycleDefaultsToml {
                shadow_allowed: Some(false),
                ..Default::default()
            }),
            rules: Vec::new(),
        }
    }

    fn incumbent_identity_key(config: &Config) -> String {
        model_router_candidate_identity_key(&ModelRouterCandidateToml {
            id: Some("incumbent".to_string()),
            model: config.model.clone(),
            model_provider: Some(config.model_provider_id.clone()),
            ..Default::default()
        })
    }

    async fn record_shadow(
        runtime: &StateRuntime,
        task_key: &str,
        phase: &str,
        candidate_identity: &str,
        base_candidate_identity: &str,
        success: bool,
        created_at_ms: i64,
    ) {
        runtime
            .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
                id: None,
                created_at_ms,
                task_key: task_key.to_string(),
                phase: phase.to_string(),
                candidate_identity: candidate_identity.to_string(),
                base_candidate_identity: base_candidate_identity.to_string(),
                success,
                score: Some(if success { 1.0 } else { 0.0 }),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 1,
                outcome: None,
                metadata_json: None,
            })
            .await
            .expect("record shadow");
    }

    async fn lifecycle_events(
        runtime: &StateRuntime,
        task_key: &str,
    ) -> Vec<codex_state::ModelRouterLifecycleEventRecord> {
        runtime
            .model_router_lifecycle_events(codex_state::ModelRouterLifecycleStatsQuery {
                window_start_ms: None,
                window_end_ms: i64::MAX,
                task_key: Some(task_key.to_string()),
                candidate_identity: None,
                event_limit: 50,
            })
            .await
            .expect("lifecycle events")
    }
}
