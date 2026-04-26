use std::collections::BTreeMap;

use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::config_toml::ModelPolicyRouteToml;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::protocol::SubAgentSource;

use crate::config::Config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelPolicySource {
    SubAgent(SubAgentSource),
    Module(&'static str),
}

impl ModelPolicySource {
    fn candidates(&self) -> Vec<String> {
        match self {
            ModelPolicySource::SubAgent(source) => {
                let specifics = match source {
                    SubAgentSource::Review => vec!["review".to_string()],
                    SubAgentSource::Compact => vec!["compact".to_string()],
                    SubAgentSource::MemoryConsolidation => {
                        vec!["memory_consolidation".to_string()]
                    }
                    SubAgentSource::ThreadSpawn { agent_role, .. } => {
                        let mut specifics = vec!["thread_spawn".to_string()];
                        if let Some(role) = agent_role.as_deref() {
                            specifics.push(format!("thread_spawn.{role}"));
                        }
                        specifics
                    }
                    SubAgentSource::Other(source) => vec![source.clone()],
                };
                let mut candidates = Vec::new();
                for specific in specifics {
                    candidates.push(format!("subagent.{specific}"));
                }
                candidates.push("subagent".to_string());
                candidates.push("agent".to_string());
                candidates
            }
            ModelPolicySource::Module(module) => {
                vec![format!("module.{module}"), (*module).to_string()]
            }
        }
    }
}

pub(crate) fn apply_model_policy(
    config: &mut Config,
    source: ModelPolicySource,
    prompt_bytes: usize,
) -> Result<(), String> {
    let Some(model_policy) = config.model_policy.as_ref() else {
        return Ok(());
    };
    if !model_policy.enabled {
        return Ok(());
    }

    let source_candidates = source.candidates();
    let route = model_policy
        .rules
        .iter()
        .find(|rule| {
            let source_matches = rule.source.as_ref().is_none_or(|sources| {
                sources.iter().any(|source| {
                    source == "*"
                        || source_candidates
                            .iter()
                            .any(|candidate| candidate == source)
                })
            });
            let min_matches = rule.min_prompt_bytes.is_none_or(|min| prompt_bytes >= min);
            let max_matches = rule.max_prompt_bytes.is_none_or(|max| prompt_bytes <= max);
            source_matches && min_matches && max_matches
        })
        .map(|rule| rule.route.clone())
        .or_else(|| model_policy.default_route.clone());

    let Some(route) = route else {
        return Ok(());
    };
    let mut policy_config = config.clone();
    apply_route(&mut policy_config, &route)?;
    *config = policy_config;
    Ok(())
}

fn apply_route(config: &mut Config, route: &ModelPolicyRouteToml) -> Result<(), String> {
    if let Some(model_provider_id) = &route.model_provider {
        let model_provider = config
            .model_providers
            .get(model_provider_id)
            .ok_or_else(|| {
                format!(
                    "model_policy route references unknown model_provider `{model_provider_id}`"
                )
            })?
            .clone();
        config.model_provider_id = model_provider_id.clone();
        config.model_provider = model_provider;
    }
    if let Some(model) = &route.model {
        config.model = Some(model.clone());
    }
    if let Some(reasoning_effort) = route
        .reasoning_effort
        .and_then(codex_config::config_toml::ModelPolicyReasoningEffortToml::as_reasoning_effort)
    {
        config.model_reasoning_effort = Some(reasoning_effort);
    }
    if let Some(account_pool) = &route.account_pool {
        set_account_pool(config, account_pool)?;
    }
    if let Some(account) = &route.account {
        set_single_account(config, account);
    }
    Ok(())
}

fn set_account_pool(config: &mut Config, account_pool: &str) -> Result<(), String> {
    let configured = config
        .account_pool
        .as_mut()
        .ok_or_else(|| format!("model_policy route references account_pool `{account_pool}`, but [account_pool] is not configured"))?;
    if !configured.pools.contains_key(account_pool) {
        return Err(format!(
            "model_policy route references unknown account_pool `{account_pool}`"
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
    use codex_config::config_toml::ModelPolicyRuleToml;
    use codex_config::config_toml::ModelPolicyToml;
    use codex_protocol::openai_models::ReasoningEffort;

    use super::*;
    use crate::config;

    #[tokio::test]
    async fn routes_subagent_by_source_and_prompt_size() {
        let mut config = config::test_config().await;
        config.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["subagent".to_string()]),
                max_prompt_bytes: Some(100),
                route: ModelPolicyRouteToml {
                    model: Some("gpt-5.3-codex-spark".to_string()),
                    reasoning_effort: Some(
                        codex_config::config_toml::ModelPolicyReasoningEffortToml::Low,
                    ),
                    account: Some("spark-account".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });

        apply_model_policy(
            &mut config,
            ModelPolicySource::SubAgent(SubAgentSource::Review),
            80,
        )
        .expect("policy should apply");

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
    async fn leaves_non_matching_large_prompt_unchanged() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["subagent".to_string()]),
                max_prompt_bytes: Some(100),
                route: ModelPolicyRouteToml {
                    model: Some("small-model".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });

        apply_model_policy(
            &mut config,
            ModelPolicySource::SubAgent(SubAgentSource::Review),
            101,
        )
        .expect("policy should not fail");

        assert_eq!(config.model.as_deref(), Some("parent-model"));
    }

    #[tokio::test]
    async fn matches_wildcard_source_and_inherits_reasoning() {
        let mut config = config::test_config().await;
        config.model = Some("parent-model".to_string());
        config.model_reasoning_effort = Some(ReasoningEffort::High);
        config.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["*".to_string()]),
                route: ModelPolicyRouteToml {
                    model: Some("small-model".to_string()),
                    reasoning_effort: Some(
                        codex_config::config_toml::ModelPolicyReasoningEffortToml::Inherit,
                    ),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });

        apply_model_policy(
            &mut config,
            ModelPolicySource::SubAgent(SubAgentSource::MemoryConsolidation),
            1,
        )
        .expect("policy should apply");

        assert_eq!(config.model.as_deref(), Some("small-model"));
        assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    }

    #[tokio::test]
    async fn leaves_config_unchanged_when_route_fails() {
        let mut config = config::test_config().await;
        config.model = Some("default-model".to_string());
        config.model_reasoning_effort = Some(ReasoningEffort::Medium);
        config.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["subagent.review".to_string()]),
                route: ModelPolicyRouteToml {
                    model: Some("policy-model".to_string()),
                    account_pool: Some("missing-pool".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });

        let err = apply_model_policy(
            &mut config,
            ModelPolicySource::SubAgent(SubAgentSource::Review),
            1,
        )
        .expect_err("unknown account pool should fail");

        assert!(err.contains("account_pool"));
        assert_eq!(config.model.as_deref(), Some("default-model"));
        assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::Medium));
    }
}
