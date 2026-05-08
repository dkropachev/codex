use std::collections::BTreeSet;
use std::fmt;

use codex_config::config_toml::ModelRouterCandidateToml;
use codex_config::config_toml::ModelRouterDiscoveryToml;
use codex_config::config_toml::ModelRouterLifecycleDefaultsToml;
use codex_config::config_toml::ModelRouterLifecycleRuleToml;
use codex_config::config_toml::ModelRouterModelRuleTypeToml;
use codex_config::config_toml::ModelRouterModelSelectorToml;
use codex_config::config_toml::ModelRouterToml;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;

use crate::ModelRouterCandidateIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyAvailableModel {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRoute {
    pub index: usize,
    pub model_provider: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRouteDecision {
    pub route_index: usize,
    pub score_bias: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyApplication {
    pub routes: Vec<PolicyRouteDecision>,
    pub matched_require_rules: Vec<String>,
    pub matched_exclude_rules: Vec<String>,
    pub matched_bias_rules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveLifecycle {
    pub window: String,
    pub cost_budget_usd: f64,
    pub token_budget: u64,
    pub min_evaluated: u64,
    pub min_confidence: f64,
    pub min_success_rate: f64,
    pub shadow_allowed: bool,
    pub promotion_shadow_sample_rate_limit: f64,
    pub monitoring_shadow_sample_rate_limit: f64,
    pub auto_promote: bool,
    pub auto_demote: bool,
    pub matched_rule_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    InvalidRegex { value: String, message: String },
    NoEligibleRoutes { task_key: String },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegex { value, message } => {
                write!(
                    formatter,
                    "invalid model_router regex selector `{value}`: {message}"
                )
            }
            Self::NoEligibleRoutes { task_key } => {
                write!(
                    formatter,
                    "model_router policy left no eligible routes for `{task_key}`"
                )
            }
        }
    }
}

impl std::error::Error for PolicyError {}

#[derive(Debug, Clone)]
enum Selector {
    Exact(String),
    Regex(Regex),
}

impl Selector {
    fn parse(value: &str) -> Result<Self, PolicyError> {
        if let Some(pattern) = regex_selector_pattern(value) {
            return Regex::new(pattern)
                .map(Self::Regex)
                .map_err(|err| PolicyError::InvalidRegex {
                    value: value.to_string(),
                    message: err.to_string(),
                });
        }
        Ok(Self::Exact(value.to_string()))
    }

    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Exact(expected) => value == expected,
            Self::Regex(regex) => regex.is_match(value),
        }
    }

    fn exact_value(&self) -> Option<&str> {
        match self {
            Self::Exact(value) => Some(value.as_str()),
            Self::Regex(_) => None,
        }
    }
}

pub fn candidate_pool_for_discovery(
    model_router: &ModelRouterToml,
    explicit_candidates: &[ModelRouterCandidateToml],
    curated_candidates: Vec<ModelRouterCandidateToml>,
    available_models: &[PolicyAvailableModel],
    default_provider: &str,
) -> Result<Vec<ModelRouterCandidateToml>, PolicyError> {
    match model_router.discovery.unwrap_or_default() {
        ModelRouterDiscoveryToml::Curated => {
            let mut candidates = explicit_candidates.to_vec();
            candidates.extend(curated_candidates);
            Ok(candidates)
        }
        ModelRouterDiscoveryToml::Manual => Ok(explicit_candidates.to_vec()),
        ModelRouterDiscoveryToml::FromRules => {
            candidates_from_rules(model_router, available_models, default_provider)
        }
    }
}

pub fn candidates_from_rules(
    model_router: &ModelRouterToml,
    available_models: &[PolicyAvailableModel],
    default_provider: &str,
) -> Result<Vec<ModelRouterCandidateToml>, PolicyError> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    let mut selectors = Vec::new();
    if let Some(models) = model_router.models.as_ref() {
        for rule in &models.rules {
            selectors.extend(rule.models.iter().cloned());
        }
    }
    if let Some(bias) = model_router.bias.as_ref() {
        for rule in &bias.rules {
            selectors.extend(rule.models.iter().cloned());
        }
    }
    if let Some(lifecycle) = model_router.lifecycle.as_ref() {
        for rule in &lifecycle.rules {
            selectors.extend(rule.models.iter().cloned());
        }
    }

    for selector in selectors {
        for candidate in candidates_for_selector(&selector, available_models, default_provider)? {
            let key = format!(
                "{}\u{1f}{}",
                candidate
                    .model_provider
                    .as_deref()
                    .unwrap_or(default_provider),
                candidate.model.as_deref().unwrap_or_default()
            );
            if seen.insert(key) {
                candidates.push(candidate);
            }
        }
    }
    Ok(candidates)
}

pub fn candidate_identity(candidate: &ModelRouterCandidateToml) -> ModelRouterCandidateIdentity {
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

pub fn candidate_identity_key(candidate: &ModelRouterCandidateToml) -> String {
    serde_json::to_string(&candidate_identity(candidate)).unwrap_or_else(|_| "{}".to_string())
}

pub fn apply_model_router_policy(
    model_router: &ModelRouterToml,
    task_key: &str,
    routes: &[PolicyRoute],
) -> Result<PolicyApplication, PolicyError> {
    let mut eligible = vec![true; routes.len()];
    let mut matched_require_rules = Vec::new();
    let mut matched_exclude_rules = Vec::new();
    let mut matched_bias_rules = Vec::new();

    if let Some(models) = model_router.models.as_ref() {
        let mut matching_require_rules = Vec::new();
        for rule in models
            .rules
            .iter()
            .filter(|rule| rule.rule_type == ModelRouterModelRuleTypeToml::Require)
        {
            if rule_matches_task(&rule.tasks, &rule.except_tasks, task_key)? {
                matching_require_rules.push(rule);
            }
        }
        if !matching_require_rules.is_empty() {
            eligible.fill(false);
            for rule in matching_require_rules {
                matched_require_rules.push(rule_label(rule.id.as_deref()));
                for (index, route) in routes.iter().enumerate() {
                    if model_selectors_match_route(&rule.models, route)? {
                        eligible[index] = true;
                    }
                }
            }
        }

        for rule in models
            .rules
            .iter()
            .filter(|rule| rule.rule_type == ModelRouterModelRuleTypeToml::Exclude)
        {
            if !rule_matches_task(&rule.tasks, &rule.except_tasks, task_key)? {
                continue;
            }
            matched_exclude_rules.push(rule_label(rule.id.as_deref()));
            for (index, route) in routes.iter().enumerate() {
                if eligible[index] && model_selectors_match_route(&rule.models, route)? {
                    eligible[index] = false;
                }
            }
        }
    }

    let hard_rules_matched = !matched_require_rules.is_empty() || !matched_exclude_rules.is_empty();
    if hard_rules_matched && !eligible.iter().any(|is_eligible| *is_eligible) {
        return Err(PolicyError::NoEligibleRoutes {
            task_key: task_key.to_string(),
        });
    }

    let mut score_biases = vec![0.0; routes.len()];
    if let Some(bias) = model_router.bias.as_ref() {
        for rule in &bias.rules {
            if !rule_matches_task(&rule.tasks, &rule.except_tasks, task_key)? {
                continue;
            }
            let mut matched_any_route = false;
            for (index, route) in routes.iter().enumerate() {
                if eligible[index] && model_selectors_match_route(&rule.models, route)? {
                    score_biases[index] += rule.score_bias;
                    matched_any_route = true;
                }
            }
            if matched_any_route {
                matched_bias_rules.push(rule_label(rule.id.as_deref()));
            }
        }
    }

    Ok(PolicyApplication {
        routes: routes
            .iter()
            .enumerate()
            .filter_map(|(index, route)| {
                eligible[index].then_some(PolicyRouteDecision {
                    route_index: route.index,
                    score_bias: score_biases[index],
                })
            })
            .collect(),
        matched_require_rules,
        matched_exclude_rules,
        matched_bias_rules,
    })
}

pub fn effective_lifecycle_for_route(
    model_router: Option<&ModelRouterToml>,
    task_key: &str,
    route: Option<&PolicyRoute>,
) -> Result<EffectiveLifecycle, PolicyError> {
    let mut lifecycle = EffectiveLifecycle::default();
    let Some(model_router) = model_router else {
        return Ok(lifecycle);
    };
    let Some(configured) = model_router.lifecycle.as_ref() else {
        return Ok(lifecycle);
    };
    if let Some(defaults) = configured.defaults.as_ref() {
        apply_lifecycle_defaults(&mut lifecycle, defaults);
    }
    for rule in lifecycle_rules_matching(configured, task_key, route)? {
        apply_lifecycle_rule(&mut lifecycle, rule);
        lifecycle.matched_rule_ids.push(rule.id.clone());
    }
    Ok(lifecycle)
}

impl Default for EffectiveLifecycle {
    fn default() -> Self {
        Self {
            window: "30d".to_string(),
            cost_budget_usd: 10.0,
            token_budget: 1_000_000,
            min_evaluated: 20,
            min_confidence: 0.8,
            min_success_rate: 0.9,
            shadow_allowed: true,
            promotion_shadow_sample_rate_limit: 0.05,
            monitoring_shadow_sample_rate_limit: 0.02,
            auto_promote: true,
            auto_demote: true,
            matched_rule_ids: Vec::new(),
        }
    }
}

fn lifecycle_rules_matching<'a>(
    lifecycle: &'a codex_config::config_toml::ModelRouterLifecycleToml,
    task_key: &str,
    route: Option<&PolicyRoute>,
) -> Result<Vec<&'a ModelRouterLifecycleRuleToml>, PolicyError> {
    let mut rules = Vec::new();
    for rule in &lifecycle.rules {
        if !rule_matches_task(&rule.tasks, &rule.except_tasks, task_key)? {
            continue;
        }
        if rule.models.is_empty() {
            rules.push(rule);
            continue;
        }
        if let Some(route) = route
            && model_selectors_match_route(&rule.models, route)?
        {
            rules.push(rule);
        }
    }
    Ok(rules)
}

fn candidates_for_selector(
    selector: &ModelRouterModelSelectorToml,
    available_models: &[PolicyAvailableModel],
    default_provider: &str,
) -> Result<Vec<ModelRouterCandidateToml>, PolicyError> {
    let provider_selector = selector
        .provider
        .as_deref()
        .map(Selector::parse)
        .transpose()?;
    let model_selector = selector.model.as_deref().map(Selector::parse).transpose()?;
    let provider_is_regex = matches!(provider_selector, Some(Selector::Regex(_)));
    let model_is_regex = matches!(model_selector, Some(Selector::Regex(_)));

    if provider_is_regex || model_is_regex {
        return Ok(available_models
            .iter()
            .filter(|available| {
                provider_selector
                    .as_ref()
                    .is_none_or(|selector| selector.matches(&available.provider))
                    && model_selector
                        .as_ref()
                        .is_none_or(|selector| selector.matches(&available.model))
            })
            .map(|available| {
                candidate_for_provider_model(&available.provider, Some(&available.model))
            })
            .collect());
    }

    let provider = provider_selector
        .as_ref()
        .and_then(Selector::exact_value)
        .unwrap_or(default_provider);
    let model = model_selector.as_ref().and_then(Selector::exact_value);
    Ok(vec![candidate_for_provider_model(provider, model)])
}

fn candidate_for_provider_model(provider: &str, model: Option<&str>) -> ModelRouterCandidateToml {
    ModelRouterCandidateToml {
        model_provider: Some(provider.to_string()),
        model: model.map(str::to_string),
        ..Default::default()
    }
}

fn rule_matches_task(
    tasks: &[String],
    except_tasks: &[String],
    task_key: &str,
) -> Result<bool, PolicyError> {
    let included = tasks.is_empty() || selectors_match_value(tasks, task_key)?;
    Ok(included && !selectors_match_value(except_tasks, task_key)?)
}

fn selector_matches_value(selector: &str, value: &str) -> Result<bool, PolicyError> {
    Ok(Selector::parse(selector)?.matches(value))
}

fn selectors_match_value(selectors: &[String], value: &str) -> Result<bool, PolicyError> {
    for selector in selectors {
        if selector_matches_value(selector, value)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn model_selectors_match_route(
    selectors: &[ModelRouterModelSelectorToml],
    route: &PolicyRoute,
) -> Result<bool, PolicyError> {
    for selector in selectors {
        if model_selector_matches_route(selector, route)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn model_selector_matches_route(
    selector: &ModelRouterModelSelectorToml,
    route: &PolicyRoute,
) -> Result<bool, PolicyError> {
    let provider_matches = selector
        .provider
        .as_deref()
        .map(|provider| selector_matches_value(provider, &route.model_provider))
        .transpose()?
        .unwrap_or(true);
    if !provider_matches {
        return Ok(false);
    }
    selector
        .model
        .as_deref()
        .map(|model| {
            route
                .model
                .as_deref()
                .map(|route_model| selector_matches_value(model, route_model))
                .unwrap_or(Ok(false))
        })
        .transpose()
        .map(|matches| matches.unwrap_or(true))
}

fn apply_lifecycle_defaults(
    lifecycle: &mut EffectiveLifecycle,
    defaults: &ModelRouterLifecycleDefaultsToml,
) {
    if let Some(window) = &defaults.window {
        lifecycle.window = window.clone();
    }
    if let Some(cost_budget_usd) = defaults.cost_budget_usd {
        lifecycle.cost_budget_usd = cost_budget_usd;
    }
    if let Some(token_budget) = defaults.token_budget {
        lifecycle.token_budget = token_budget;
    }
    if let Some(min_evaluated) = defaults.min_evaluated {
        lifecycle.min_evaluated = min_evaluated;
    }
    if let Some(min_confidence) = defaults.min_confidence {
        lifecycle.min_confidence = min_confidence;
    }
    if let Some(min_success_rate) = defaults.min_success_rate {
        lifecycle.min_success_rate = min_success_rate;
    }
    if let Some(shadow_allowed) = defaults.shadow_allowed {
        lifecycle.shadow_allowed = shadow_allowed;
    }
    if let Some(rate_limit) = defaults.promotion_shadow_sample_rate_limit {
        lifecycle.promotion_shadow_sample_rate_limit = rate_limit;
    }
    if let Some(rate_limit) = defaults.monitoring_shadow_sample_rate_limit {
        lifecycle.monitoring_shadow_sample_rate_limit = rate_limit;
    }
    if let Some(auto_promote) = defaults.auto_promote {
        lifecycle.auto_promote = auto_promote;
    }
    if let Some(auto_demote) = defaults.auto_demote {
        lifecycle.auto_demote = auto_demote;
    }
}

fn apply_lifecycle_rule(lifecycle: &mut EffectiveLifecycle, rule: &ModelRouterLifecycleRuleToml) {
    if let Some(window) = &rule.window {
        lifecycle.window = window.clone();
    }
    if let Some(cost_budget_usd) = rule.cost_budget_usd {
        lifecycle.cost_budget_usd = cost_budget_usd;
    }
    if let Some(token_budget) = rule.token_budget {
        lifecycle.token_budget = token_budget;
    }
    if let Some(min_evaluated) = rule.min_evaluated {
        lifecycle.min_evaluated = min_evaluated;
    }
    if let Some(min_confidence) = rule.min_confidence {
        lifecycle.min_confidence = min_confidence;
    }
    if let Some(min_success_rate) = rule.min_success_rate {
        lifecycle.min_success_rate = min_success_rate;
    }
    if let Some(shadow_allowed) = rule.shadow_allowed {
        lifecycle.shadow_allowed = shadow_allowed;
    }
    if let Some(rate_limit) = rule.promotion_shadow_sample_rate_limit {
        lifecycle.promotion_shadow_sample_rate_limit = rate_limit;
    }
    if let Some(rate_limit) = rule.monitoring_shadow_sample_rate_limit {
        lifecycle.monitoring_shadow_sample_rate_limit = rate_limit;
    }
    if let Some(auto_promote) = rule.auto_promote {
        lifecycle.auto_promote = auto_promote;
    }
    if let Some(auto_demote) = rule.auto_demote {
        lifecycle.auto_demote = auto_demote;
    }
}

fn rule_label(id: Option<&str>) -> String {
    id.unwrap_or("<unnamed>").to_string()
}

fn regex_selector_pattern(value: &str) -> Option<&str> {
    (value.len() >= 2 && value.starts_with('/') && value.ends_with('/'))
        .then_some(&value[1..value.len() - 1])
}

#[cfg(test)]
mod tests {
    use codex_config::config_toml::ModelRouterBiasRuleToml;
    use codex_config::config_toml::ModelRouterBiasToml;
    use codex_config::config_toml::ModelRouterDiscoveryToml;
    use codex_config::config_toml::ModelRouterLifecycleToml;
    use codex_config::config_toml::ModelRouterModelRuleToml;
    use codex_config::config_toml::ModelRouterModelsToml;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn from_rules_expands_regex_against_available_models() {
        let router = ModelRouterToml {
            discovery: Some(ModelRouterDiscoveryToml::FromRules),
            models: Some(ModelRouterModelsToml {
                rules: vec![ModelRouterModelRuleToml {
                    id: Some("top".to_string()),
                    rule_type: ModelRouterModelRuleTypeToml::Require,
                    tasks: vec!["/review$/".to_string()],
                    except_tasks: Vec::new(),
                    models: vec![ModelRouterModelSelectorToml {
                        provider: Some("openai".to_string()),
                        model: Some("/^gpt-5\\.5/".to_string()),
                    }],
                }],
            }),
            ..Default::default()
        };
        let candidates = candidate_pool_for_discovery(
            &router,
            &[],
            Vec::new(),
            &[
                PolicyAvailableModel {
                    provider: "openai".to_string(),
                    model: "gpt-5.5".to_string(),
                },
                PolicyAvailableModel {
                    provider: "openai".to_string(),
                    model: "gpt-5.3-codex-spark".to_string(),
                },
            ],
            "openai",
        )
        .expect("candidate pool");

        assert_eq!(
            candidates,
            vec![ModelRouterCandidateToml {
                model_provider: Some("openai".to_string()),
                model: Some("gpt-5.5".to_string()),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn require_rules_union_then_exclude_rules_subtract() {
        let router = ModelRouterToml {
            models: Some(ModelRouterModelsToml {
                rules: vec![
                    ModelRouterModelRuleToml {
                        id: Some("top".to_string()),
                        rule_type: ModelRouterModelRuleTypeToml::Require,
                        tasks: vec!["/review$/".to_string()],
                        except_tasks: Vec::new(),
                        models: vec![ModelRouterModelSelectorToml {
                            provider: Some("openai".to_string()),
                            model: Some("/^gpt-5/".to_string()),
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
        };

        let application = apply_model_router_policy(
            &router,
            "subagent.review",
            &[
                route(/*index*/ 0, "openai", "gpt-4o"),
                route(/*index*/ 1, "openai", "gpt-5.5"),
                route(/*index*/ 2, "openai", "gpt-5.3-codex-spark"),
            ],
        )
        .expect("policy application");

        assert_eq!(
            application.routes,
            vec![PolicyRouteDecision {
                route_index: 1,
                score_bias: 0.0,
            }]
        );
        assert_eq!(application.matched_require_rules, vec!["top"]);
        assert_eq!(application.matched_exclude_rules, vec!["no-spark"]);
    }

    #[test]
    fn bias_rules_apply_to_still_eligible_routes() {
        let router = ModelRouterToml {
            bias: Some(ModelRouterBiasToml {
                rules: vec![ModelRouterBiasRuleToml {
                    id: Some("spark".to_string()),
                    tasks: vec!["module.repo_ci.triage".to_string()],
                    except_tasks: Vec::new(),
                    models: vec![ModelRouterModelSelectorToml {
                        provider: Some("openai".to_string()),
                        model: Some("/spark/".to_string()),
                    }],
                    score_bias: 0.15,
                }],
            }),
            ..Default::default()
        };

        let application = apply_model_router_policy(
            &router,
            "module.repo_ci.triage",
            &[
                route(/*index*/ 0, "openai", "gpt-5.5"),
                route(/*index*/ 1, "openai", "gpt-5.3-codex-spark"),
            ],
        )
        .expect("policy application");

        assert_eq!(
            application.routes,
            vec![
                PolicyRouteDecision {
                    route_index: 0,
                    score_bias: 0.0,
                },
                PolicyRouteDecision {
                    route_index: 1,
                    score_bias: 0.15,
                },
            ]
        );
        assert_eq!(application.matched_bias_rules, vec!["spark"]);
    }

    #[test]
    fn lifecycle_rules_inherit_defaults_and_override_set_fields() {
        let router = ModelRouterToml {
            lifecycle: Some(ModelRouterLifecycleToml {
                defaults: Some(ModelRouterLifecycleDefaultsToml {
                    window: Some("7d".to_string()),
                    min_confidence: Some(0.7),
                    min_success_rate: Some(0.8),
                    ..Default::default()
                }),
                rules: vec![ModelRouterLifecycleRuleToml {
                    id: "review".to_string(),
                    tasks: vec!["/review$/".to_string()],
                    except_tasks: Vec::new(),
                    models: Vec::new(),
                    window: None,
                    cost_budget_usd: None,
                    token_budget: None,
                    min_evaluated: Some(40),
                    min_confidence: Some(0.9),
                    min_success_rate: None,
                    shadow_allowed: None,
                    promotion_shadow_sample_rate_limit: None,
                    monitoring_shadow_sample_rate_limit: None,
                    auto_promote: None,
                    auto_demote: None,
                }],
            }),
            ..Default::default()
        };

        let lifecycle = effective_lifecycle_for_route(Some(&router), "subagent.review", None)
            .expect("lifecycle");

        assert_eq!(lifecycle.window, "7d");
        assert_eq!(lifecycle.min_evaluated, 40);
        assert_eq!(lifecycle.min_confidence, 0.9);
        assert_eq!(lifecycle.min_success_rate, 0.8);
        assert_eq!(lifecycle.matched_rule_ids, vec!["review"]);
    }

    fn route(index: usize, provider: &str, model: &str) -> PolicyRoute {
        PolicyRoute {
            index,
            model_provider: provider.to_string(),
            model: Some(model.to_string()),
        }
    }
}
