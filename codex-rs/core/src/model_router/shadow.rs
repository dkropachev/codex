use codex_config::config_toml::ModelRouterCandidateToml;
use codex_model_router::policy;
use codex_model_router::policy::PolicyRoute;
use codex_state::ModelRouterLifecyclePromotionRecord;
use codex_state::ModelRouterMetricOverlay;
use codex_state::ModelRouterShadowEvaluationSummary;
use codex_state::StateRuntime;

use crate::config::Config;

use super::AvailableRouterModel;
use super::LIFECYCLE_PHASE_MONITORING;
use super::LIFECYCLE_PHASE_PROMOTION;
use super::LIFECYCLE_STATUS_PROMOTED;
use super::ModelRouterRouteExclusion;
use super::build_candidate_set;
use super::candidate_for_selectable_route;
use super::lifecycle_cost_budget_usd_micros;
use super::lifecycle_window_start_ms;
use super::load_lifecycle_promotions;
use super::load_metric_overlays;
use super::model_router_candidate_identity_key;

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

    let overlays = load_metric_overlays(config, Some(state_db)).await;
    let promotions = load_lifecycle_promotions(task_key, Some(state_db)).await;
    model_router_shadow_plan_with_state(
        config,
        task_key,
        prompt_bytes,
        available_models,
        exclusions,
        &overlays,
        &promotions,
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
    state_db: &StateRuntime,
) -> Option<ModelRouterShadowPlan> {
    let model_router = config.model_router.as_ref()?;
    let candidate_set = match build_candidate_set(
        config,
        task_key,
        prompt_bytes,
        available_models,
        overlays,
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

    policy_application
        .routes
        .iter()
        .enumerate()
        .filter_map(|(policy_order, decision)| {
            let selectable_route = selectable_routes.get(decision.route_index)?;
            let candidate = candidate_for_selectable_route(&candidate_set, selectable_route)?;
            let route = policy_routes.get(decision.route_index)?;
            let lifecycle =
                policy::effective_lifecycle_for_route(Some(model_router), task_key, Some(route))
                    .ok()?;
            if !lifecycle.shadow_allowed {
                return None;
            }

            let candidate_identity = model_router_candidate_identity_key(candidate);
            let promoted = promotions.iter().find(|promotion| {
                promotion.task_key == task_key
                    && promotion.candidate_identity == candidate_identity
                    && promotion
                        .status
                        .eq_ignore_ascii_case(LIFECYCLE_STATUS_PROMOTED)
            });
            let phase = if promoted.is_some() {
                LIFECYCLE_PHASE_MONITORING
            } else {
                LIFECYCLE_PHASE_PROMOTION
            };
            let summary = matching_summary(&summaries, phase, &candidate_identity);
            if shadow_budget_exhausted(summary, &lifecycle)
                || summary.is_some_and(|summary| {
                    summary.evaluated_count
                        >= i64::try_from(lifecycle.min_evaluated).unwrap_or(i64::MAX)
                })
            {
                return None;
            }

            Some(ShadowCandidate {
                plan: ModelRouterShadowPlan {
                    task_key: task_key.to_string(),
                    phase: phase.to_string(),
                    candidate: candidate.clone(),
                    candidate_identity,
                    base_candidate_identity: promoted
                        .map(|promotion| promotion.base_candidate_identity.clone())
                        .unwrap_or_else(|| base_candidate_identity.clone()),
                },
                evaluated_count: summary.map_or(0, |summary| summary.evaluated_count),
                policy_order,
            })
        })
        .min_by(|left, right| {
            left.evaluated_count
                .cmp(&right.evaluated_count)
                .then_with(|| left.policy_order.cmp(&right.policy_order))
        })
        .map(|candidate| candidate.plan)
}

fn shadow_budget_exhausted(
    summary: Option<&ModelRouterShadowEvaluationSummary>,
    lifecycle: &policy::EffectiveLifecycle,
) -> bool {
    let Some(summary) = summary else {
        return false;
    };
    summary.tokens_used >= i64::try_from(lifecycle.token_budget).unwrap_or(i64::MAX)
        || summary.cost_used_usd_micros >= lifecycle_cost_budget_usd_micros(lifecycle)
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
            lifecycle: Some(lifecycle_with_min_evaluated(2)),
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
            lifecycle: Some(lifecycle_with_min_evaluated(1)),
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

    fn available_model(model_provider_id: &str, model: &str) -> AvailableRouterModel {
        AvailableRouterModel {
            model_provider_id: model_provider_id.to_string(),
            model: model.to_string(),
            context_window: Some(272_000),
            max_context_window: Some(272_000),
            effective_context_window_percent: 95,
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
        runtime
            .record_model_router_shadow_evaluation(ModelRouterShadowEvaluationRecord {
                id: None,
                created_at_ms: 1,
                task_key: task_key.to_string(),
                phase: LIFECYCLE_PHASE_PROMOTION.to_string(),
                candidate_identity: candidate_identity.to_string(),
                base_candidate_identity: base_candidate_identity.to_string(),
                success: true,
                score: Some(1.0),
                confidence: 1.0,
                cost_usd_micros: 0,
                total_tokens: 1,
                outcome: None,
                metadata_json: None,
            })
            .await
            .expect("record shadow");
    }
}
