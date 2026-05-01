use std::collections::BTreeMap;
use std::sync::Arc;

mod auto_candidates;
mod failover;

use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_login::AuthManager;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_router::CandidateMetrics;
use codex_model_router::CandidateRoute;
use codex_model_router::ModelRouterCandidateIdentity;
use codex_model_router::RouterTaskClass;
use codex_model_router::TokenPrice;
use codex_model_router::estimate_task_usage;
use codex_model_router::estimate_token_cost;
use codex_model_router::select_candidate;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::SubAgentSource;
use codex_state::ModelRouterMetricOverlay;
use codex_state::StateRuntime;

use crate::config::Config;

pub(crate) use failover::ModelRouterAppliedRoute;
pub(crate) use failover::ModelRouterRouteExclusion;
pub(crate) use failover::model_router_failure_scope;
use failover::selectable_routes;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelRouterSource {
    SubAgent(SubAgentSource),
    Module(&'static str),
}

impl ModelRouterSource {
    pub(crate) fn task_key(&self) -> String {
        match self {
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
        exclusions,
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
    apply_model_router_with_overlays_and_exclusions(
        config,
        source,
        prompt_bytes,
        available_models,
        &overlays,
        exclusions,
    )
}

fn apply_model_router_with_overlays_and_exclusions(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    overlays: &[ModelRouterMetricOverlay],
    exclusions: &[ModelRouterRouteExclusion],
) -> Result<Option<ModelRouterAppliedRoute>, String> {
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(None);
    };
    if !model_router.enabled {
        return Ok(None);
    }

    let task_key = source.task_key();
    let candidate_set =
        build_candidate_set(config, &task_key, prompt_bytes, available_models, overlays);
    tracing::debug!(
        task_key = task_key,
        candidates = model_router.candidates.len(),
        auto_candidates = candidate_set.auto_candidate_count,
        "evaluating model router"
    );

    let selectable_routes = selectable_routes(config, &candidate_set, exclusions);
    let filtered_routes = selectable_routes
        .iter()
        .map(|route| route.route.clone())
        .collect::<Vec<_>>();
    let Some(selection) = select_candidate(&task_key, prompt_bytes, &filtered_routes) else {
        return Ok(None);
    };
    let selected_route_index = selection.index;
    let selected_route = selectable_routes
        .get(selected_route_index)
        .ok_or_else(|| format!("model_router selected missing filtered route index {selected_route_index}"))?;
    let selected_index = selected_route.index;
    tracing::debug!(
        task_key = task_key,
        selected_index,
        score = selection.score,
        task_class = ?selection.task_class,
        "selected model router candidate"
    );
    let applied_route = ModelRouterAppliedRoute {
        task_key,
        route: selected_route.key.clone(),
    };
    if selected_index == 0 {
        return Ok(Some(applied_route));
    }
    let Some(candidate) = candidate_set.candidates.get(selected_index - 1) else {
        return Err(format!(
            "model_router selected missing candidate index {selected_index}"
        ));
    };
    let mut router_config = config.clone();
    apply_candidate(&mut router_config, candidate)?;
    *config = router_config;
    Ok(Some(applied_route))
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

pub(crate) fn auth_manager_for_config(
    config: &Config,
    parent: &Arc<AuthManager>,
) -> Arc<AuthManager> {
    if config_account_pool_default(config) == parent.default_account_pool_id() {
        return Arc::clone(parent);
    }
    AuthManager::shared_from_config_with_parent_auth(config, parent)
}

fn config_account_pool_default(config: &Config) -> Option<String> {
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
    model: String,
    context_window: Option<i64>,
    max_context_window: Option<i64>,
    effective_context_window_percent: i64,
}

impl AvailableRouterModel {
    fn from_model_info(model_info: &ModelInfo) -> Self {
        Self {
            model: model_info.slug.clone(),
            context_window: model_info.context_window,
            max_context_window: model_info.max_context_window,
            effective_context_window_percent: model_info.effective_context_window_percent,
        }
    }

    fn without_context(model: String) -> Self {
        Self {
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

pub(crate) fn available_router_models(
    models_manager: &SharedModelsManager,
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
                .map(AvailableRouterModel::from_model_info)
                .unwrap_or_else(|| AvailableRouterModel::without_context(preset.model))
        })
        .collect()
}

struct CandidateSet {
    routes: Vec<CandidateRoute>,
    candidates: Vec<ModelRouterCandidateToml>,
    auto_candidate_count: usize,
}

fn build_candidate_set(
    config: &Config,
    task_key: &str,
    prompt_bytes: usize,
    available_models: &[AvailableRouterModel],
    overlays: &[ModelRouterMetricOverlay],
) -> CandidateSet {
    let Some(model_router) = config.model_router.as_ref() else {
        return CandidateSet {
            routes: Vec::new(),
            candidates: Vec::new(),
            auto_candidate_count: 0,
        };
    };
    let auto_candidates =
        auto_candidates::candidates_from_available_models(config, available_models);
    let auto_candidate_count = auto_candidates.len();
    let mut candidates = Vec::with_capacity(model_router.candidates.len() + auto_candidate_count);
    candidates.extend(model_router.candidates.iter().cloned());
    candidates.extend(auto_candidates);

    let mut routes = Vec::with_capacity(candidates.len() + 1);
    routes.push(CandidateRoute {
        id: Some("incumbent".to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        usable_context_window_tokens: usable_context_window_tokens(
            config,
            config.model.as_deref(),
            available_models,
        ),
        is_incumbent: true,
        metrics: CandidateMetrics::default(),
    });
    let task_class = RouterTaskClass::infer(task_key, prompt_bytes);
    routes.extend(candidates.iter().map(|candidate| {
        let overlay = overlay_for_candidate(candidate, overlays);
        CandidateRoute {
            id: candidate.id.clone(),
            model: candidate.model.clone().or_else(|| config.model.clone()),
            model_provider: candidate
                .model_provider
                .clone()
                .or_else(|| Some(config.model_provider_id.clone())),
            usable_context_window_tokens: usable_context_window_tokens(
                config,
                candidate.model.as_deref().or(config.model.as_deref()),
                available_models,
            ),
            is_incumbent: false,
            metrics: candidate_metrics(candidate, task_class, prompt_bytes, overlay),
        }
    }));
    CandidateSet {
        routes,
        candidates,
        auto_candidate_count,
    }
}

fn usable_context_window_tokens(
    config: &Config,
    model: Option<&str>,
    available_models: &[AvailableRouterModel],
) -> Option<i64> {
    let Some(model) = model else {
        return configured_usable_context_window_tokens(config);
    };
    available_models
        .iter()
        .find(|available_model| available_model.model == model)
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
    use codex_config::config_toml::ModelRouterCandidateToml;
    use codex_config::config_toml::ModelRouterReasoningEffortToml;
    use codex_config::config_toml::ModelRouterToml;
    use codex_login::AuthManager;
    use codex_protocol::config_types::ServiceTier;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_state::ModelRouterMetricOverlay;
    use codex_state::StateRuntime;
    use tempfile::TempDir;

    use super::*;
    use crate::config;

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
    async fn tool_router_source_uses_model_router_to_select_available_spark() {
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
            ModelRouterSource::Module("tool_router.resolve"),
            80,
            &available_models,
        )
        .expect("router should apply");

        assert_eq!(config.model.as_deref(), Some("gpt-5.3-codex-spark"));
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

    fn available_model(model: &str) -> AvailableRouterModel {
        available_model_with_context(model, Some(272_000), Some(272_000))
    }

    fn available_model_with_context(
        model: &str,
        context_window: Option<i64>,
        max_context_window: Option<i64>,
    ) -> AvailableRouterModel {
        AvailableRouterModel {
            model: model.to_string(),
            context_window,
            max_context_window,
            effective_context_window_percent: 95,
        }
    }

    async fn state_runtime() -> (TempDir, std::sync::Arc<StateRuntime>) {
        let codex_home = TempDir::new().expect("temp dir");
        let runtime = StateRuntime::init(codex_home.path().to_path_buf(), "openai".to_string())
            .await
            .expect("state runtime");
        (codex_home, runtime)
    }
}
