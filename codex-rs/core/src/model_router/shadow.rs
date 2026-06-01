use std::cmp::Ordering;
use std::collections::BTreeMap;

use codex_config::config_toml::ModelRouterCandidateToml;
use codex_model_router::policy;
use codex_model_router::policy::PolicyRoute;
use codex_state::MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO;
use codex_state::ModelRouterLifecyclePromotionRecord;
use codex_state::ModelRouterLifecycleTransitionContext;
use codex_state::ModelRouterMetricOverlay;
use codex_state::ModelRouterShadowEvaluationSummary;
use codex_state::StateRuntime;

use crate::config::Config;

use super::AvailableRouterModel;
use super::LIFECYCLE_PHASE_MONITORING;
use super::LIFECYCLE_PHASE_PROMOTION;
use super::LIFECYCLE_STATUS_EVALUATING;
use super::LIFECYCLE_STATUS_PROMOTED;
use super::LIFECYCLE_STATUS_REJECTED;
use super::ModelRouterRouteExclusion;
use super::build_candidate_set;
use super::candidate_for_selectable_route;
use super::lifecycle_shadow_budget_exhausted;
use super::lifecycle_summary_has_enough_samples;
use super::lifecycle_window_start_ms;
use super::load_lifecycle_promotions;
use super::load_metric_overlays;
use super::load_route_max_observed_total_tokens;
use super::model_provider_and_model_from_identity_key;
use super::model_router_candidate_identity_key;
use super::route_fits_current_request;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelRouterShadowPlan {
    pub(crate) task_key: String,
    pub(crate) phase: String,
    pub(crate) candidate: ModelRouterCandidateToml,
    pub(crate) candidate_identity: String,
    pub(crate) base_candidate_identity: String,
}

#[derive(Debug)]
struct ShadowCandidate {
    plan: ModelRouterShadowPlan,
    evaluated_count: i64,
    policy_order: usize,
    provider_order: usize,
    choice: ShadowChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ShadowChoice {
    Bootstrap,
    Promotion,
    Monitoring,
}

#[derive(Debug)]
struct EligibleShadowCandidate {
    plan: ModelRouterShadowPlan,
    lifecycle: policy::EffectiveLifecycle,
    status: Option<String>,
    production_model_provider: String,
    production_model: Option<String>,
    promotion_summary: Option<ModelRouterShadowEvaluationSummary>,
    monitoring_summary: Option<ModelRouterShadowEvaluationSummary>,
    policy_order: usize,
}

pub(crate) async fn model_router_shadow_plan(
    config: &Config,
    task_key: &str,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    state_db: Option<&StateRuntime>,
    exclusions: &[ModelRouterRouteExclusion],
) -> Option<ModelRouterShadowPlan> {
    let state_db = state_db?;
    let model_router = config.model_router.as_ref()?;
    if !model_router.enabled {
        return None;
    }

    let overlays = load_metric_overlays(config, available_models, Some(state_db)).await;
    let promotions = load_lifecycle_promotions(task_key, Some(state_db)).await;
    let route_max_observed_total_tokens =
        load_route_max_observed_total_tokens(task_key, Some(state_db)).await;
    model_router_shadow_plan_with_state(
        config,
        task_key,
        prompt_bytes,
        available_models,
        exclusions,
        &overlays,
        &promotions,
        route_max_observed_total_tokens,
        state_db,
    )
    .await
}

async fn model_router_shadow_plan_with_state(
    config: &Config,
    task_key: &str,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    exclusions: &[ModelRouterRouteExclusion],
    overlays: &[ModelRouterMetricOverlay],
    promotions: &[ModelRouterLifecyclePromotionRecord],
    route_max_observed_total_tokens: Option<i64>,
    state_db: &StateRuntime,
) -> Option<ModelRouterShadowPlan> {
    let model_router = config.model_router.as_ref()?;
    let candidate_set = match build_candidate_set(
        config,
        task_key,
        prompt_bytes,
        available_models,
        overlays,
        promotions,
        &[],
        route_max_observed_total_tokens,
    ) {
        Ok(candidate_set) => candidate_set,
        Err(err) => {
            tracing::debug!(task_key, error = %err, "failed to build model router shadow candidate set");
            return None;
        }
    };
    let selectable_routes = super::failover::selectable_routes(config, &candidate_set, exclusions);
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
            tracing::debug!(task_key, error = %err, "failed to apply model router policy for shadow plan");
            return None;
        }
    };

    let window_start_ms = lifecycle_window_start_ms(
        &policy::effective_lifecycle_for_route(Some(model_router), task_key, /*route*/ None)
            .map(|lifecycle| lifecycle.window)
            .unwrap_or_else(|_| "30d".to_string()),
    );
    let summaries = match state_db
        .model_router_shadow_evaluation_summaries_since(Some(task_key), window_start_ms)
        .await
    {
        Ok(summaries) => summaries,
        Err(err) => {
            tracing::debug!(task_key, error = %err, "failed to load model router shadow summaries");
            return None;
        }
    };
    let base_candidate_identity = incumbent_identity_key(config);

    let mut providers = BTreeMap::<String, Vec<EligibleShadowCandidate>>::new();
    let mut provider_orders = BTreeMap::<String, usize>::new();
    for (policy_order, decision) in policy_application.routes.iter().enumerate() {
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
        let lifecycle = match policy::effective_lifecycle_for_route(
            Some(model_router),
            task_key,
            Some(route),
        ) {
            Ok(lifecycle) => lifecycle,
            Err(err) => {
                tracing::debug!(task_key, error = %err, "failed to resolve model router lifecycle for shadow plan");
                continue;
            }
        };
        if !lifecycle.shadow_allowed
            || !candidate_set.route_history_fits(selectable_route.index)
            || !route_fits_current_request(task_key, prompt_bytes, &selectable_route.route)
        {
            continue;
        }

        let candidate_identity = model_router_candidate_identity_key(candidate);
        let status = promotions
            .iter()
            .find(|promotion| {
                promotion.task_key == task_key && promotion.candidate_identity == candidate_identity
            })
            .map(|promotion| promotion.status.clone());
        if status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case(LIFECYCLE_STATUS_REJECTED))
        {
            continue;
        }
        let base_candidate_identity = promotions
            .iter()
            .find(|promotion| {
                promotion.task_key == task_key && promotion.candidate_identity == candidate_identity
            })
            .map(|promotion| promotion.base_candidate_identity.clone())
            .unwrap_or_else(|| base_candidate_identity.clone());
        let provider = selectable_route.key.model_provider.clone();
        provider_orders
            .entry(provider.clone())
            .or_insert(policy_order);
        providers
            .entry(provider)
            .or_default()
            .push(EligibleShadowCandidate {
                plan: ModelRouterShadowPlan {
                    task_key: task_key.to_string(),
                    phase: if status.as_deref().is_some_and(|status| {
                        status.eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED)
                    }) {
                        LIFECYCLE_PHASE_MONITORING.to_string()
                    } else {
                        LIFECYCLE_PHASE_PROMOTION.to_string()
                    },
                    candidate: candidate.clone(),
                    candidate_identity: candidate_identity.clone(),
                    base_candidate_identity,
                },
                lifecycle,
                status,
                production_model_provider: selectable_route.key.model_provider.clone(),
                production_model: selectable_route.key.model.clone(),
                promotion_summary: matching_summary(
                    &summaries,
                    LIFECYCLE_PHASE_PROMOTION,
                    &candidate_identity,
                )
                .cloned(),
                monitoring_summary: matching_summary(
                    &summaries,
                    LIFECYCLE_PHASE_MONITORING,
                    &candidate_identity,
                )
                .cloned(),
                policy_order,
            });
    }

    let mut choices = Vec::new();
    for (provider, candidates) in providers {
        let provider_order = provider_orders
            .get(&provider)
            .copied()
            .unwrap_or(usize::MAX);
        if let Some(candidate) = evaluating_candidate(&candidates) {
            if let Some(choice) = promotion_shadow_choice(candidate, provider_order) {
                choices.push(choice);
            }
            continue;
        }
        if let Some(candidate) = bootstrap_candidate(&candidates, provider_order) {
            choices.push(candidate);
            continue;
        }
        if let Some(candidate) =
            start_evaluating_candidate(state_db, task_key, &candidates, provider_order).await
        {
            choices.push(candidate);
            continue;
        }
        if let Some(candidate) = monitoring_candidate(&candidates, provider_order) {
            choices.push(candidate);
        }
    }

    choices
        .into_iter()
        .min_by(compare_shadow_candidates)
        .map(|candidate| candidate.plan)
}

fn shadow_budget_exhausted(
    summary: Option<&ModelRouterShadowEvaluationSummary>,
    lifecycle: &policy::EffectiveLifecycle,
) -> bool {
    let Some(summary) = summary else {
        return false;
    };
    lifecycle_shadow_budget_exhausted(summary, lifecycle)
}

fn evaluating_candidate(
    candidates: &[EligibleShadowCandidate],
) -> Option<&EligibleShadowCandidate> {
    candidates.iter().find(|candidate| {
        candidate
            .status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case(LIFECYCLE_STATUS_EVALUATING))
    })
}

fn promotion_shadow_choice(
    candidate: &EligibleShadowCandidate,
    provider_order: usize,
) -> Option<ShadowCandidate> {
    let summary = candidate.promotion_summary.as_ref();
    if shadow_budget_exhausted(summary, &candidate.lifecycle)
        || summary.is_some_and(|summary| {
            lifecycle_summary_has_enough_samples(summary, &candidate.lifecycle)
        })
    {
        return None;
    }
    Some(ShadowCandidate {
        plan: candidate.plan.clone(),
        evaluated_count: summary.map_or(0, |summary| summary.evaluated_count),
        policy_order: candidate.policy_order,
        provider_order,
        choice: ShadowChoice::Promotion,
    })
}

fn bootstrap_candidate(
    candidates: &[EligibleShadowCandidate],
    provider_order: usize,
) -> Option<ShadowCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            !candidate
                .status
                .as_deref()
                .is_some_and(|status| status.eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED))
                && candidate.promotion_summary.is_none()
        })
        .min_by_key(|candidate| candidate.policy_order)
        .map(|candidate| ShadowCandidate {
            plan: candidate.plan.clone(),
            evaluated_count: 0,
            policy_order: candidate.policy_order,
            provider_order,
            choice: ShadowChoice::Bootstrap,
        })
}

async fn start_evaluating_candidate(
    state_db: &StateRuntime,
    task_key: &str,
    candidates: &[EligibleShadowCandidate],
    provider_order: usize,
) -> Option<ShadowCandidate> {
    let candidate = candidates
        .iter()
        .filter(|candidate| {
            !candidate.status.as_deref().is_some_and(|status| {
                status.eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED)
                    || status.eq_ignore_ascii_case(LIFECYCLE_STATUS_REJECTED)
                    || status.eq_ignore_ascii_case(LIFECYCLE_STATUS_EVALUATING)
            }) && candidate.promotion_summary.is_some()
        })
        .max_by(|left, right| compare_evaluation_candidates(left, right))?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let summary = candidate.promotion_summary.as_ref();
    let (base_model_provider, base_model) =
        model_provider_and_model_from_identity_key(&candidate.plan.base_candidate_identity);
    if let Err(err) = state_db
        .mark_model_router_lifecycle_candidate_evaluating(
            ModelRouterLifecyclePromotionRecord {
                task_key: task_key.to_string(),
                candidate_identity: candidate.plan.candidate_identity.clone(),
                base_candidate_identity: candidate.plan.base_candidate_identity.clone(),
                status: LIFECYCLE_STATUS_EVALUATING.to_string(),
                rule_id: candidate.lifecycle.matched_rule_ids.first().cloned(),
                production_model_provider: Some(candidate.production_model_provider.clone()),
                production_model: candidate.production_model.clone(),
                base_model_provider,
                base_model,
                promoted_at_ms: now_ms,
                updated_at_ms: now_ms,
                reason: Some("started promotion shadow evaluation".to_string()),
            },
            ModelRouterLifecycleTransitionContext {
                source: MODEL_ROUTER_LIFECYCLE_SOURCE_AUTO.to_string(),
                lifecycle_window: Some(candidate.lifecycle.window.clone()),
                shadow_phase: Some(LIFECYCLE_PHASE_PROMOTION.to_string()),
                shadow_summary: summary.cloned(),
                failed_gates_json: None,
            },
        )
        .await
    {
        tracing::debug!(
            task_key,
            candidate_identity = candidate.plan.candidate_identity,
            error = %err,
            "failed to mark model router candidate evaluating"
        );
        return None;
    }

    promotion_shadow_choice(candidate, provider_order)
}

fn monitoring_candidate(
    candidates: &[EligibleShadowCandidate],
    provider_order: usize,
) -> Option<ShadowCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate
                .status
                .as_deref()
                .is_some_and(|status| status.eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED))
        })
        .filter_map(|candidate| {
            let summary = candidate.monitoring_summary.as_ref();
            if shadow_budget_exhausted(summary, &candidate.lifecycle)
                || summary.is_some_and(|summary| {
                    lifecycle_summary_has_enough_samples(summary, &candidate.lifecycle)
                })
            {
                return None;
            }
            let mut plan = candidate.plan.clone();
            plan.phase = LIFECYCLE_PHASE_MONITORING.to_string();
            Some(ShadowCandidate {
                plan,
                evaluated_count: summary.map_or(0, |summary| summary.evaluated_count),
                policy_order: candidate.policy_order,
                provider_order,
                choice: ShadowChoice::Monitoring,
            })
        })
        .min_by(compare_shadow_candidates)
}

fn compare_shadow_candidates(left: &ShadowCandidate, right: &ShadowCandidate) -> Ordering {
    left.choice
        .cmp(&right.choice)
        .then_with(|| left.evaluated_count.cmp(&right.evaluated_count))
        .then_with(|| left.provider_order.cmp(&right.provider_order))
        .then_with(|| left.policy_order.cmp(&right.policy_order))
}

fn compare_evaluation_candidates(
    left: &EligibleShadowCandidate,
    right: &EligibleShadowCandidate,
) -> Ordering {
    shadow_quality_score(left)
        .total_cmp(&shadow_quality_score(right))
        .then_with(|| shadow_confidence(left).total_cmp(&shadow_confidence(right)))
        .then_with(|| shadow_success_rate(left).total_cmp(&shadow_success_rate(right)))
        .then_with(|| right.policy_order.cmp(&left.policy_order))
}

fn shadow_quality_score(candidate: &EligibleShadowCandidate) -> f64 {
    candidate
        .promotion_summary
        .as_ref()
        .and_then(|summary| summary.average_score)
        .unwrap_or(f64::NEG_INFINITY)
}

fn shadow_confidence(candidate: &EligibleShadowCandidate) -> f64 {
    candidate
        .promotion_summary
        .as_ref()
        .map(|summary| summary.average_confidence)
        .unwrap_or(f64::NEG_INFINITY)
}

fn shadow_success_rate(candidate: &EligibleShadowCandidate) -> f64 {
    candidate
        .promotion_summary
        .as_ref()
        .map(|summary| summary.success_rate)
        .unwrap_or(f64::NEG_INFINITY)
}

fn matching_summary<'a>(
    summaries: &'a [ModelRouterShadowEvaluationSummary],
    phase: &str,
    candidate_identity: &str,
) -> Option<&'a ModelRouterShadowEvaluationSummary> {
    summaries
        .iter()
        .find(|summary| summary.phase == phase && summary.candidate_identity == candidate_identity)
}

fn incumbent_identity_key(config: &Config) -> String {
    model_router_candidate_identity_key(&ModelRouterCandidateToml {
        id: Some("incumbent".to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use codex_config::config_toml::ModelRouterLifecycleDefaultsToml;
    use codex_config::config_toml::ModelRouterLifecycleToml;
    use codex_config::config_toml::ModelRouterToml;
    use codex_state::ModelRouterShadowEvaluationRecord;
    use codex_state::StateRuntime;
    use tempfile::TempDir;

    use super::*;

    #[tokio::test]
    async fn plans_auto_discovered_candidate_without_explicit_config() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            ..Default::default()
        });
        let available_models = vec![available_model("openai", "gpt-5.3-codex-spark")];

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &available_models,
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("shadow plan");

        assert_eq!(plan.task_key, "chat.default");
        assert_eq!(plan.phase, LIFECYCLE_PHASE_PROMOTION);
        assert_eq!(plan.candidate.model.as_deref(), Some("gpt-5.3-codex-spark"));
    }

    #[tokio::test]
    async fn plans_candidate_with_fewest_shadow_samples() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 2)),
            ..Default::default()
        });
        let available_models = vec![
            available_model("openai", "gpt-5.3-codex-spark"),
            available_model("deepseek", "deepseek-v4-flash"),
        ];
        let spark = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            model_provider: Some("openai".to_string()),
            ..Default::default()
        };
        record_shadow(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&spark),
            &incumbent_identity_key(&config),
        )
        .await;

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &available_models,
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("shadow plan");

        assert_eq!(plan.candidate.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[tokio::test]
    async fn stops_after_candidate_reaches_minimum_samples() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 1)),
            ..Default::default()
        });
        let available_models = vec![available_model("openai", "gpt-5.3-codex-spark")];
        let spark = ModelRouterCandidateToml {
            id: Some("spark".to_string()),
            model: Some("gpt-5.3-codex-spark".to_string()),
            model_provider: Some("openai".to_string()),
            ..Default::default()
        };
        record_shadow(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&spark),
            &incumbent_identity_key(&config),
        )
        .await;

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &available_models,
            Some(runtime.as_ref()),
            &[],
        )
        .await;

        assert_eq!(plan, None);
    }

    #[tokio::test]
    async fn bootstraps_every_eligible_candidate_once_before_evaluation() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        let first = candidate("first", "openai", "gpt-5.3-codex-spark");
        let second = candidate("second", "openai", "gpt-5.4-mini");
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 2)),
            candidates: vec![first.clone(), second.clone()],
            ..Default::default()
        });

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("first bootstrap");
        assert_eq!(
            plan.candidate_identity,
            model_router_candidate_identity_key(&first)
        );
        record_shadow(
            runtime.as_ref(),
            "chat.default",
            &plan.candidate_identity,
            &plan.base_candidate_identity,
        )
        .await;

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("second bootstrap");

        assert_eq!(
            plan.candidate_identity,
            model_router_candidate_identity_key(&second)
        );
    }

    #[tokio::test]
    async fn starts_provider_local_evaluation_by_shadow_quality() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        let weak = candidate("weak", "openai", "gpt-5.4-mini");
        let strong = candidate("strong", "openai", "gpt-5.3-codex-spark");
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 2)),
            candidates: vec![weak.clone(), strong.clone()],
            ..Default::default()
        });
        record_shadow_with_score(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&weak),
            &base_identity,
            /*success*/ true,
            /*score*/ Some(0.2),
            /*confidence*/ 1.0,
        )
        .await;
        record_shadow_with_score(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&strong),
            &base_identity,
            /*success*/ true,
            /*score*/ Some(0.9),
            /*confidence*/ 0.8,
        )
        .await;

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("evaluation plan");

        assert_eq!(
            plan.candidate_identity,
            model_router_candidate_identity_key(&strong)
        );
        assert_eq!(
            runtime
                .model_router_lifecycle_promotions(Some("chat.default"))
                .await
                .expect("lifecycle")
                .first()
                .map(|promotion| promotion.status.as_str()),
            Some(LIFECYCLE_STATUS_EVALUATING)
        );
    }

    #[tokio::test]
    async fn evaluating_candidate_repeats_until_min_evaluated() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        let candidate = candidate("spark", "openai", "gpt-5.3-codex-spark");
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 3)),
            candidates: vec![candidate.clone()],
            ..Default::default()
        });
        record_shadow_with_score(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&candidate),
            &base_identity,
            /*success*/ true,
            /*score*/ Some(0.9),
            /*confidence*/ 1.0,
        )
        .await;
        let first = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("first evaluation plan");
        let second = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("repeated evaluation plan");

        assert_eq!(first.candidate_identity, second.candidate_identity);
        assert_eq!(
            second.candidate_identity,
            model_router_candidate_identity_key(&candidate)
        );
    }

    #[tokio::test]
    async fn provider_cycles_are_independent() {
        let (_codex_home, runtime) = state_runtime().await;
        let mut config = crate::config::test_config().await;
        config.model = Some("gpt-5.5".to_string());
        let openai = candidate("spark", "openai", "gpt-5.3-codex-spark");
        let deepseek = candidate("deepseek", "deepseek", "deepseek-v4-flash");
        let base_identity = incumbent_identity_key(&config);
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            lifecycle: Some(lifecycle_with_min_evaluated(/*min_evaluated*/ 3)),
            candidates: vec![openai.clone()],
            ..Default::default()
        });
        record_shadow_with_score(
            runtime.as_ref(),
            "chat.default",
            &model_router_candidate_identity_key(&openai),
            &base_identity,
            /*success*/ true,
            /*score*/ Some(0.9),
            /*confidence*/ 1.0,
        )
        .await;
        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("openai evaluation plan");
        assert_eq!(
            plan.candidate_identity,
            model_router_candidate_identity_key(&openai)
        );
        config
            .model_router
            .as_mut()
            .expect("router")
            .candidates
            .push(deepseek.clone());

        let plan = model_router_shadow_plan(
            &config,
            "chat.default",
            /*prompt_bytes*/ 10_000,
            &[],
            Some(runtime.as_ref()),
            &[],
        )
        .await
        .expect("deepseek bootstrap plan");

        assert_eq!(
            plan.candidate_identity,
            model_router_candidate_identity_key(&deepseek)
        );
    }

    fn available_model(model_provider_id: &str, model: &str) -> AvailableRouterModel {
        AvailableRouterModel {
            model_provider_id: model_provider_id.to_string(),
            model: model.to_string(),
            context_window: Some(272_000),
            max_context_window: Some(272_000),
            effective_context_window_percent: 95,
        }
    }

    fn candidate(id: &str, provider: &str, model: &str) -> ModelRouterCandidateToml {
        ModelRouterCandidateToml {
            id: Some(id.to_string()),
            model: Some(model.to_string()),
            model_provider: Some(provider.to_string()),
            ..Default::default()
        }
    }

    fn lifecycle_with_min_evaluated(min_evaluated: u64) -> ModelRouterLifecycleToml {
        ModelRouterLifecycleToml {
            defaults: Some(ModelRouterLifecycleDefaultsToml {
                window: Some("all".to_string()),
                min_evaluated: Some(min_evaluated),
                ..Default::default()
            }),
            rules: Vec::new(),
        }
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }

    async fn record_shadow(
        runtime: &StateRuntime,
        task_key: &str,
        candidate_identity: &str,
        base_candidate_identity: &str,
    ) {
        record_shadow_with_score(
            runtime,
            task_key,
            candidate_identity,
            base_candidate_identity,
            /*success*/ true,
            /*score*/ Some(1.0),
            /*confidence*/ 1.0,
        )
        .await;
    }

    async fn record_shadow_with_score(
        runtime: &StateRuntime,
        task_key: &str,
        candidate_identity: &str,
        base_candidate_identity: &str,
        success: bool,
        score: Option<f64>,
        confidence: f64,
    ) {
        runtime
            .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
                id: None,
                created_at_ms: 1,
                task_key: task_key.to_string(),
                phase: LIFECYCLE_PHASE_PROMOTION.to_string(),
                candidate_identity: candidate_identity.to_string(),
                base_candidate_identity: base_candidate_identity.to_string(),
                success,
                score,
                confidence,
                cost_usd_micros: 0,
                total_tokens: 1,
                outcome: None,
                metadata_json: None,
            })
            .await
            .expect("record shadow");
    }
}
