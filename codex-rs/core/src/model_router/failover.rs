use codex_model_router::CandidateRoute;
use codex_protocol::error::CodexErr;

use super::CandidateSet;
use super::config_account_pool_default;
use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelRouterFailureScope {
    Provider,
    Model,
    Account,
    Route,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRouterAppliedRoute {
    pub(crate) task_key: String,
    pub(crate) route: ModelRouterRouteKey,
}

impl ModelRouterAppliedRoute {
    pub(crate) fn exclusion_for_failure(
        &self,
        scope: ModelRouterFailureScope,
    ) -> ModelRouterRouteExclusion {
        ModelRouterRouteExclusion::from_failure_scope(&self.route, scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRouterRouteKey {
    pub(crate) model_provider: String,
    pub(crate) model: Option<String>,
    pub(crate) account_pool: Option<String>,
    pub(crate) account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelRouterRouteExclusion {
    Provider {
        model_provider: String,
    },
    Model {
        model_provider: String,
        model: Option<String>,
    },
    Account {
        model_provider: String,
        account_pool: Option<String>,
        account: Option<String>,
    },
    Route(ModelRouterRouteKey),
}

impl ModelRouterRouteExclusion {
    pub(crate) fn from_failure_scope(
        route: &ModelRouterRouteKey,
        scope: ModelRouterFailureScope,
    ) -> Self {
        match scope {
            ModelRouterFailureScope::Provider => Self::Provider {
                model_provider: route.model_provider.clone(),
            },
            ModelRouterFailureScope::Model => Self::Model {
                model_provider: route.model_provider.clone(),
                model: route.model.clone(),
            },
            ModelRouterFailureScope::Account => {
                if route.account_pool.is_some() || route.account.is_some() {
                    Self::Account {
                        model_provider: route.model_provider.clone(),
                        account_pool: route.account_pool.clone(),
                        account: route.account.clone(),
                    }
                } else {
                    Self::Provider {
                        model_provider: route.model_provider.clone(),
                    }
                }
            }
            ModelRouterFailureScope::Route => Self::Route(route.clone()),
        }
    }

    pub(super) fn excludes(&self, route: &ModelRouterRouteKey) -> bool {
        match self {
            Self::Provider { model_provider } => route.model_provider == *model_provider,
            Self::Model {
                model_provider,
                model,
            } => route.model_provider == *model_provider && route.model == *model,
            Self::Account {
                model_provider,
                account_pool,
                account,
            } => {
                route.model_provider == *model_provider
                    && route.account_pool == *account_pool
                    && route.account == *account
            }
            Self::Route(excluded) => route == excluded,
        }
    }
}

pub(crate) fn model_router_failure_scope(err: &CodexErr) -> Option<ModelRouterFailureScope> {
    match err {
        CodexErr::Stream(message, _) => message_failure_scope(message),
        CodexErr::Timeout | CodexErr::ContextWindowExceeded | CodexErr::ServerOverloaded => {
            Some(ModelRouterFailureScope::Model)
        }
        CodexErr::UsageLimitReached(_) | CodexErr::UsageNotIncluded | CodexErr::QuotaExceeded => {
            Some(ModelRouterFailureScope::Account)
        }
        CodexErr::UnexpectedStatus(_)
        | CodexErr::InternalServerError
        | CodexErr::RetryLimit(_)
        | CodexErr::ConnectionFailed(_)
        | CodexErr::ResponseStreamFailed(_) => Some(ModelRouterFailureScope::Provider),
        CodexErr::InvalidRequest(message) => message_failure_scope(message),
        CodexErr::TurnAborted
        | CodexErr::Interrupted
        | CodexErr::EnvVar(_)
        | CodexErr::Fatal(_)
        | CodexErr::InvalidImageRequest()
        | CodexErr::RefreshTokenFailed(_)
        | CodexErr::UnsupportedOperation(_)
        | CodexErr::Sandbox(_)
        | CodexErr::LandlockSandboxExecutableNotProvided
        | CodexErr::ThreadNotFound(_)
        | CodexErr::AgentLimitReached { .. }
        | CodexErr::Spawn
        | CodexErr::SessionConfiguredNotFirstEvent
        | CodexErr::CyberPolicy { .. }
        | CodexErr::InternalAgentDied
        | CodexErr::Io(_)
        | CodexErr::Json(_)
        | CodexErr::TokioJoin(_) => None,
        #[cfg(target_os = "linux")]
        CodexErr::LandlockRuleset(_) | CodexErr::LandlockPathFd(_) => None,
    }
}

fn message_failure_scope(message: &str) -> Option<ModelRouterFailureScope> {
    let message = message.to_ascii_lowercase();
    if message.contains("rate_limit_exceeded") || message.contains("rate limit reached") {
        return Some(ModelRouterFailureScope::Model);
    }
    if message.contains("context_window_exceeded")
        || message.contains("context length")
        || message.contains("context window")
        || message.contains("maximum context")
        || message.contains("too many tokens")
    {
        return Some(ModelRouterFailureScope::Model);
    }
    if message.contains("idle timeout") {
        return Some(ModelRouterFailureScope::Model);
    }
    if message.contains("network")
        || message.contains("dns")
        || message.contains("connection")
        || message.contains("connect")
        || message.contains("tls")
    {
        return Some(ModelRouterFailureScope::Provider);
    }
    if message.contains("timeout") {
        return Some(ModelRouterFailureScope::Model);
    }
    if message.trim().is_empty() {
        None
    } else {
        Some(ModelRouterFailureScope::Route)
    }
}

pub(super) struct SelectableRoute {
    pub(super) index: usize,
    pub(super) route: CandidateRoute,
    pub(super) key: ModelRouterRouteKey,
}

pub(super) fn selectable_routes(
    config: &Config,
    candidate_set: &CandidateSet,
    exclusions: &[ModelRouterRouteExclusion],
) -> Vec<SelectableRoute> {
    candidate_set
        .routes
        .iter()
        .enumerate()
        .filter_map(|(index, route)| {
            let key = route_key_for_index(config, candidate_set, index, route);
            if exclusions.iter().any(|exclusion| exclusion.excludes(&key)) {
                None
            } else {
                Some(SelectableRoute {
                    index,
                    route: route.clone(),
                    key,
                })
            }
        })
        .collect()
}

fn route_key_for_index(
    config: &Config,
    candidate_set: &CandidateSet,
    index: usize,
    route: &CandidateRoute,
) -> ModelRouterRouteKey {
    let candidate = index
        .checked_sub(1)
        .and_then(|candidate_index| candidate_set.candidates.get(candidate_index));
    ModelRouterRouteKey {
        model_provider: route
            .model_provider
            .clone()
            .unwrap_or_else(|| config.model_provider_id.clone()),
        model: route.model.clone().or_else(|| config.model.clone()),
        account_pool: candidate
            .and_then(|candidate| candidate.account_pool.clone())
            .or_else(|| {
                (index == 0)
                    .then(|| config_account_pool_default(config))
                    .flatten()
            }),
        account: candidate.and_then(|candidate| candidate.account.clone()),
    }
}

#[cfg(test)]
mod tests {
    use codex_config::config_toml::ModelRouterCandidateToml;
    use codex_config::config_toml::ModelRouterToml;
    use codex_protocol::error::CodexErr;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::config;
    use crate::model_router::ModelRouterSource;
    use crate::model_router::apply_model_router_with_exclusions;

    #[tokio::test]
    async fn exclusions_skip_failed_model_and_select_next_route() {
        let mut base = config::test_config().await;
        base.model = Some("gpt-5.4".to_string());
        base.model_router = Some(ModelRouterToml {
            enabled: true,
            candidates: vec![
                ModelRouterCandidateToml {
                    id: Some("fast".to_string()),
                    model: Some("gpt-5.3-codex-spark".to_string()),
                    intelligence_score: Some(0.80),
                    success_rate: Some(0.99),
                    median_latency_ms: Some(1_000),
                    ..Default::default()
                },
                ModelRouterCandidateToml {
                    id: Some("backup".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    intelligence_score: Some(0.70),
                    success_rate: Some(0.99),
                    median_latency_ms: Some(2_000),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        let mut first = base.clone();
        let route = apply_model_router_with_exclusions(
            &mut first,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            &[],
        )
        .expect("router should apply")
        .expect("router should select a route");
        assert_eq!(first.model.as_deref(), Some("gpt-5.3-codex-spark"));

        let mut failover = base;
        let exclusion = route.exclusion_for_failure(ModelRouterFailureScope::Model);
        let route = apply_model_router_with_exclusions(
            &mut failover,
            ModelRouterSource::Module("repo_ci.triage"),
            80,
            &[],
            &[exclusion],
        )
        .expect("router should apply")
        .expect("router should select a route");

        assert_eq!(failover.model.as_deref(), Some("gpt-5-mini"));
        assert_eq!(route.route.model.as_deref(), Some("gpt-5-mini"));
    }

    #[test]
    fn classifies_failure_scope_by_error_kind() {
        assert_eq!(
            model_router_failure_scope(&CodexErr::ContextWindowExceeded),
            Some(ModelRouterFailureScope::Model)
        );
        assert_eq!(
            model_router_failure_scope(&CodexErr::Stream(
                "Rate limit reached for gpt-5.1. Please try again in 1s.".to_string(),
                None,
            )),
            Some(ModelRouterFailureScope::Model)
        );
        assert_eq!(
            model_router_failure_scope(&CodexErr::Stream(
                "network error: connection reset".to_string(),
                None,
            )),
            Some(ModelRouterFailureScope::Provider)
        );
        assert_eq!(
            model_router_failure_scope(&CodexErr::InvalidRequest(
                "context length exceeded".to_string(),
            )),
            Some(ModelRouterFailureScope::Model)
        );
        assert_eq!(
            model_router_failure_scope(&CodexErr::UsageNotIncluded),
            Some(ModelRouterFailureScope::Account)
        );
    }

    #[test]
    fn account_scope_falls_back_to_provider_without_account_route() {
        let route = ModelRouterRouteKey {
            model_provider: "openai".to_string(),
            model: Some("gpt-5.4".to_string()),
            account_pool: None,
            account: None,
        };

        assert_eq!(
            ModelRouterRouteExclusion::from_failure_scope(&route, ModelRouterFailureScope::Account),
            ModelRouterRouteExclusion::Provider {
                model_provider: "openai".to_string(),
            }
        );
    }
}
