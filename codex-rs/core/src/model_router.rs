use std::collections::BTreeMap;
use std::sync::Arc;

mod auto_candidates;

use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::config_toml::ModelRouterCandidateToml;
use codex_login::AuthManager;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_router::CandidateMetrics;
use codex_model_router::CandidateRoute;
use codex_model_router::RouterTaskClass;
use codex_model_router::TokenPrice;
use codex_model_router::estimate_task_usage;
use codex_model_router::estimate_token_cost;
use codex_model_router::select_candidate;
use codex_models_manager::manager::SharedModelsManager;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::protocol::SubAgentSource;

use crate::config::Config;

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

pub(crate) fn apply_model_router(
    config: &mut Config,
    source: ModelRouterSource,
    prompt_bytes: usize,
    available_models: &[ModelPreset],
) -> Result<(), String> {
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(());
    };
    if !model_router.enabled {
        return Ok(());
    }

    let task_key = source.task_key();
    let candidate_set = build_candidate_set(config, &task_key, prompt_bytes, available_models);
    tracing::debug!(
        task_key = task_key,
        candidates = model_router.candidates.len(),
        auto_candidates = candidate_set.auto_candidate_count,
        "evaluating model router"
    );

    let Some(selection) = select_candidate(&task_key, prompt_bytes, &candidate_set.routes) else {
        return Ok(());
    };
    tracing::debug!(
        task_key = task_key,
        selected_index = selection.index,
        score = selection.score,
        task_class = ?selection.task_class,
        "selected model router candidate"
    );
    if selection.index == 0 {
        return Ok(());
    }
    let Some(candidate) = candidate_set.candidates.get(selection.index - 1) else {
        return Err(format!(
            "model_router selected missing candidate index {}",
            selection.index
        ));
    };
    let mut router_config = config.clone();
    apply_candidate(&mut router_config, candidate)?;
    *config = router_config;
    Ok(())
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

#[derive(Debug, Clone)]
pub(crate) struct AppliedModelRouterCandidate {
    pub(crate) config: Config,
    pub(crate) auth_scope_changed: bool,
}

pub(crate) fn apply_model_router_candidate_by_id(
    config: &Config,
    candidate_id: &str,
    available_models: &[ModelPreset],
) -> Result<Option<AppliedModelRouterCandidate>, String> {
    let Some(model_router) = config.model_router.as_ref() else {
        return Ok(None);
    };
    if !model_router.enabled {
        return Ok(None);
    }
    let Some(candidate) = model_router
        .candidates
        .iter()
        .find(|candidate| candidate.id.as_deref() == Some(candidate_id))
        .cloned()
        .or_else(|| {
            auto_candidates::candidate_from_available_model_by_id(
                config,
                available_models,
                candidate_id,
            )
        })
    else {
        return Ok(None);
    };

    let auth_scope_changed = candidate.account.is_some() || candidate.account_pool.is_some();
    let mut routed_config = config.clone();
    apply_candidate(&mut routed_config, &candidate)?;
    Ok(Some(AppliedModelRouterCandidate {
        config: routed_config,
        auth_scope_changed,
    }))
}

pub(crate) fn available_model_presets(models_manager: &SharedModelsManager) -> Vec<ModelPreset> {
    models_manager.try_list_models().unwrap_or_else(|err| {
        tracing::debug!(error = %err, "failed to read available models for model router");
        Vec::new()
    })
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
    available_models: &[ModelPreset],
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
        is_incumbent: true,
        metrics: CandidateMetrics::default(),
    });
    let task_class = RouterTaskClass::infer(task_key, prompt_bytes);
    routes.extend(candidates.iter().map(|candidate| {
        CandidateRoute {
            id: candidate.id.clone(),
            model: candidate.model.clone().or_else(|| config.model.clone()),
            model_provider: candidate
                .model_provider
                .clone()
                .or_else(|| Some(config.model_provider_id.clone())),
            is_incumbent: false,
            metrics: candidate_metrics(candidate, task_class, prompt_bytes),
        }
    }));
    CandidateSet {
        routes,
        candidates,
        auto_candidate_count,
    }
}

fn candidate_metrics(
    candidate: &ModelRouterCandidateToml,
    task_class: RouterTaskClass,
    prompt_bytes: usize,
) -> CandidateMetrics {
    let estimated_cost_usd_micros = token_price_from_candidate(candidate).map(|price| {
        estimate_token_cost(
            &estimate_task_usage(prompt_bytes, task_class),
            &price,
            /*confidence*/ 1.0,
        )
        .usd_micros
    });
    CandidateMetrics {
        intelligence_score: candidate.intelligence_score,
        success_rate: candidate.success_rate,
        median_latency_ms: candidate.median_latency_ms,
        estimated_cost_usd_micros,
    }
}

fn token_price_from_candidate(candidate: &ModelRouterCandidateToml) -> Option<TokenPrice> {
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

fn apply_candidate(
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
    use codex_protocol::openai_models::ModelPreset;
    use codex_protocol::openai_models::ReasoningEffort;

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
        let available_models = vec![model_preset("gpt-5.3-codex-spark")];

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
    async fn applies_candidate_by_id_without_mutating_source_config() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![ModelRouterCandidateToml {
                id: Some("spark".to_string()),
                model: Some("gpt-5.3-codex-spark".to_string()),
                account: Some("spark-account".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        });

        let applied = apply_model_router_candidate_by_id(&config, "spark", &[])
            .expect("candidate should apply")
            .expect("candidate");

        assert_eq!(config.model.as_deref(), Some("parent-model"));
        assert_eq!(applied.config.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert!(applied.auth_scope_changed);
    }

    #[tokio::test]
    async fn candidate_by_id_uses_available_spark_model() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: Vec::new(),
            ..Default::default()
        });
        let available_models = vec![model_preset("gpt-5.3-codex-spark")];

        let applied = apply_model_router_candidate_by_id(&config, "spark", &available_models)
            .expect("candidate lookup should succeed")
            .expect("candidate");

        assert_eq!(config.model.as_deref(), Some("parent-model"));
        assert_eq!(applied.config.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert!(!applied.auth_scope_changed);
    }

    fn model_preset(model: &str) -> ModelPreset {
        ModelPreset {
            id: model.to_string(),
            model: model.to_string(),
            display_name: model.to_string(),
            description: String::new(),
            default_reasoning_effort: ReasoningEffort::None,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            additional_speed_tiers: Vec::new(),
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            availability_nux: None,
            supported_in_api: true,
            input_modalities: Vec::new(),
        }
    }
}
