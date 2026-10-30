mod refresh;
mod routing;
mod transport;
mod usage;

use usage::MemberUsage;
#[cfg(test)]
use usage::remaining_from_rate_limit;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use std::time::Instant;

use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_protocol::account::PlanType;

use crate::CodexAuth;
use crate::outbound_proxy::AuthRouteConfig;

use super::account_pool_selection::AccountPoolAssignmentKey;
use super::account_pool_selection::AccountPoolAuthSelection;
use super::account_pool_selection::AccountPoolCacheHeat;
use super::account_pool_selection::AccountPoolCacheHint;
use super::account_pool_selection::AccountPoolOperationKind;
use super::account_pool_selection::AccountPoolSelectionContext;
use super::account_pool_selection::AccountPoolUsageBucket;
use super::account_pool_selection::DEFAULT_ACCOUNT_POOL_AFFINITY_KEY;
use super::manager::AuthConfig;
use super::manager::AuthManager;
use super::manager::RefreshTokenError;
use super::manager::UnauthorizedRecovery;

const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const ACTIVE_AFFINITY_PENALTY: i64 = 3;
const UNKNOWN_USAGE_PENALTY: i64 = 25;
const WARM_REBALANCE_LOW_REMAINING: u64 = 20;
const WARM_REBALANCE_MATERIAL_ADVANTAGE: u64 = 25;
const HOT_REBALANCE_LOW_REMAINING: u64 = 10;
const HOT_REBALANCE_MATERIAL_ADVANTAGE: u64 = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolStatus {
    pub pool_id: String,
    pub policy: AccountPoolPolicyToml,
    pub active_account_id: Option<String>,
    pub members: Vec<AccountPoolMemberStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolMemberStatus {
    pub account_id: String,
    pub email: Option<String>,
    pub plan_type: Option<PlanType>,
    pub active: bool,
    pub unavailable_reason: Option<String>,
    pub regular_remaining: Option<u64>,
    pub spark_remaining: Option<u64>,
    pub credits: Option<codex_protocol::protocol::CreditsSnapshot>,
    pub rate_limit_reset_credit_count: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolUsageRefreshReport {
    pub pools: Vec<AccountPoolUsageRefreshPoolReport>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolUsageRefreshPoolReport {
    pub pool_id: String,
    pub member_count: usize,
    pub refreshed_count: usize,
    pub usable_credentials_count: usize,
    pub problems: Vec<AccountPoolUsageRefreshProblem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountPoolUsageRefreshProblem {
    pub account_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
struct MemberRuntimeState {
    regular_exhausted: bool,
    spark_exhausted: bool,
    last_error: Option<String>,
    usage: Option<MemberUsage>,
}

#[derive(Clone, Debug, Default)]
struct AccountPoolAssignment {
    account_id: String,
    cache_hint: Option<AccountPoolCacheHint>,
}

#[derive(Debug)]
struct AccountPoolMember {
    account_id: String,
    manager: Arc<AuthManager>,
}

#[derive(Debug)]
struct AccountPool {
    pool_id: String,
    definition: AccountPoolDefinitionToml,
    members: Vec<AccountPoolMember>,
    chatgpt_base_url: Option<String>,
    auth_route_config: AuthRouteConfig,
    assignments: RwLock<HashMap<AccountPoolAssignmentKey, AccountPoolAssignment>>,
    last_assignment_key: RwLock<Option<AccountPoolAssignmentKey>>,
    last_usage_refresh_attempt: RwLock<Option<Instant>>,
    state: RwLock<HashMap<String, MemberRuntimeState>>,
}

#[derive(Debug)]
pub struct AccountPoolManager {
    default_pool: String,
    pools: HashMap<String, AccountPool>,
}

impl AccountPoolManager {
    pub async fn from_config(config: AccountPoolToml, auth_config: AuthConfig) -> Option<Self> {
        Self::from_config_with_refresh_gate(
            config,
            auth_config,
            Arc::new(tokio::sync::RwLock::new(())),
        )
        .await
    }

    pub(super) async fn from_config_with_refresh_gate(
        config: AccountPoolToml,
        auth_config: AuthConfig,
        refresh_gate: Arc<tokio::sync::RwLock<()>>,
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        let default_pool = config
            .default_pool
            .clone()
            .or_else(|| config.pools.keys().next().cloned())?;
        let mut pools = HashMap::new();
        for (pool_id, definition) in config.pools {
            let mut members = Vec::new();
            for account_id in &definition.accounts {
                let mut member_auth_config = auth_config.clone();
                member_auth_config.codex_home =
                    auth_config.codex_home.join("accounts").join(account_id);
                let mut manager = AuthManager::new_from_auth_config(
                    member_auth_config,
                    /*enable_codex_api_key_env*/ false,
                )
                .await;
                manager.pool_refresh_gate = Arc::clone(&refresh_gate);
                members.push(AccountPoolMember {
                    account_id: account_id.clone(),
                    manager: Arc::new(manager),
                });
            }
            pools.insert(
                pool_id.clone(),
                AccountPool {
                    pool_id,
                    definition,
                    members,
                    chatgpt_base_url: auth_config.chatgpt_base_url.clone(),
                    auth_route_config: auth_config.auth_route_config.clone(),
                    assignments: RwLock::new(HashMap::new()),
                    last_assignment_key: RwLock::new(None),
                    last_usage_refresh_attempt: RwLock::new(None),
                    state: RwLock::new(HashMap::new()),
                },
            );
        }

        Some(Self {
            default_pool,
            pools,
        })
    }

    pub async fn auth(&self) -> Option<CodexAuth> {
        self.auth_for_bucket(AccountPoolUsageBucket::Regular).await
    }

    pub async fn auth_for_bucket(&self, bucket: AccountPoolUsageBucket) -> Option<CodexAuth> {
        self.auth_for_context(AccountPoolSelectionContext::default_for_bucket(bucket))
            .await
            .map(|selection| selection.auth)
    }

    pub async fn auth_for_context(
        &self,
        context: AccountPoolSelectionContext,
    ) -> Option<AccountPoolAuthSelection> {
        let pool = self.pools.get(&self.default_pool)?;
        pool.refresh_stale_usage().await;
        pool.select_auth(context).await
    }

    pub fn auth_cached(&self) -> Option<CodexAuth> {
        self.cached_auth_with_owner().map(|(auth, _)| auth)
    }

    pub fn activate_cached_auth(&self) -> Option<CodexAuth> {
        let pool = self.pools.get(&self.default_pool)?;
        pool.activate_cached_auth(AccountPoolUsageBucket::Regular)
    }

    pub fn status(&self) -> Option<AccountPoolStatus> {
        self.pools.get(&self.default_pool).map(AccountPool::status)
    }

    pub fn mark_exhausted(&self, account_id: &str, bucket: AccountPoolUsageBucket, error: String) {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return;
        };
        pool.mark_exhausted(account_id, bucket, error);
    }

    pub fn active_account_id(&self) -> Option<String> {
        self.pools
            .get(&self.default_pool)
            .and_then(AccountPool::active_account_id)
    }

    pub fn mark_active_exhausted(&self, bucket: AccountPoolUsageBucket, error: String) -> bool {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return false;
        };
        let Some(account_id) = pool.active_account_id() else {
            return false;
        };
        pool.mark_exhausted(&account_id, bucket, error);
        pool.has_available_member(bucket)
    }

    pub fn mark_selection_exhausted(
        &self,
        selection: &AccountPoolAuthSelection,
        bucket: AccountPoolUsageBucket,
        error: String,
    ) -> bool {
        let Some(pool) = self.pools.get(&selection.pool_id) else {
            return false;
        };
        pool.mark_exhausted(&selection.account_id, bucket, error);
        pool.has_available_member(bucket)
    }

    pub fn unauthorized_recovery_for_selection(
        &self,
        selection: &AccountPoolAuthSelection,
    ) -> Option<UnauthorizedRecovery> {
        let pool = self.pools.get(&selection.pool_id)?;
        let member = pool.member(&selection.account_id)?;
        Some(member.manager.unauthorized_recovery())
    }

    pub fn unauthorized_recovery_for_auth(&self, auth: &CodexAuth) -> Option<UnauthorizedRecovery> {
        let account_id = auth.get_account_id()?;
        self.pools
            .values()
            .find_map(|pool| pool.member(&account_id))
            .map(|member| member.manager.unauthorized_recovery())
    }

    pub async fn auth_for_account_id(&self, account_id: &str) -> Option<CodexAuth> {
        let member = self
            .pools
            .values()
            .find_map(|pool| pool.member(account_id))?;
        member_auth(account_id, &member.manager).await
    }

    pub fn record_cache_hint(
        &self,
        bucket: AccountPoolUsageBucket,
        affinity_key: String,
        cache_hint: AccountPoolCacheHint,
    ) {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return;
        };
        pool.record_cache_hint(bucket, affinity_key, cache_hint);
    }

    pub async fn refresh_usage(&self, pool_id: Option<&str>) {
        let _ = self.refresh_usage_with_report(pool_id).await;
    }

    pub async fn refresh_usage_with_report(
        &self,
        pool_id: Option<&str>,
    ) -> AccountPoolUsageRefreshReport {
        let mut reports = Vec::new();
        match pool_id {
            Some(pool_id) => {
                if let Some(pool) = self.pools.get(pool_id) {
                    reports.push(pool.refresh_usage().await);
                }
            }
            None => {
                for pool in self.pools.values() {
                    reports.push(pool.refresh_usage().await);
                }
            }
        }
        reports.sort_by(|left, right| left.pool_id.cmp(&right.pool_id));
        AccountPoolUsageRefreshReport { pools: reports }
    }

    pub async fn refresh_active_token(&self) -> Result<(), RefreshTokenError> {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return Ok(());
        };
        pool.refresh_active_token().await
    }

    pub fn set_usage_for_testing(
        &self,
        account_id: &str,
        regular_remaining: Option<u64>,
        spark_remaining: Option<u64>,
        last_refreshed: Instant,
    ) {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return;
        };
        pool.set_usage(
            account_id,
            regular_remaining,
            spark_remaining,
            last_refreshed,
        );
    }
}

impl AccountPool {
    async fn select_auth(
        &self,
        mut context: AccountPoolSelectionContext,
    ) -> Option<AccountPoolAuthSelection> {
        if context.affinity_key.is_empty() {
            context.affinity_key = DEFAULT_ACCOUNT_POOL_AFFINITY_KEY.to_string();
        }
        let assignment_key = self.assignment_key(context.bucket, &context.affinity_key);
        let cache_hint = self.cache_hint_for_context(&context, &assignment_key);
        let member_ids = self.select_member_ids(&context, cache_hint);
        for account_id in member_ids {
            let Some((member_account_id, member_manager)) = self
                .member(&account_id)
                .map(|member| (member.account_id.clone(), Arc::clone(&member.manager)))
            else {
                continue;
            };
            let Some(auth) = member_auth(&member_account_id, &member_manager).await else {
                self.set_last_error(&member_account_id, "missing credentials".to_string());
                continue;
            };
            if !auth.is_chatgpt_auth() {
                self.set_last_error(
                    &member_account_id,
                    "account pool members must use ChatGPT auth".to_string(),
                );
                continue;
            }
            self.clear_last_error(&member_account_id);
            self.set_assignment(
                assignment_key.clone(),
                member_account_id.clone(),
                cache_hint,
            );
            return Some(AccountPoolAuthSelection {
                auth,
                pool_id: self.pool_id.clone(),
                account_id: member_account_id,
                bucket: context.bucket,
                affinity_key: context.affinity_key,
                cache_hint,
            });
        }
        None
    }

    fn activate_cached_auth(&self, bucket: AccountPoolUsageBucket) -> Option<CodexAuth> {
        let context = AccountPoolSelectionContext {
            bucket,
            affinity_key: DEFAULT_ACCOUNT_POOL_AFFINITY_KEY.to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: None,
            pinned_account_id: None,
        };
        let assignment_key = self.assignment_key(context.bucket, &context.affinity_key);
        let (account_id, auth) = self.cached_member_auth_for_context(&context)?;
        self.set_assignment(
            assignment_key.clone(),
            account_id,
            self.assignment_cache_hint(&assignment_key),
        );
        Some(auth)
    }

    fn cached_member_auth(&self, bucket: AccountPoolUsageBucket) -> Option<(String, CodexAuth)> {
        self.cached_member_auth_for_context(&AccountPoolSelectionContext {
            bucket,
            affinity_key: DEFAULT_ACCOUNT_POOL_AFFINITY_KEY.to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: None,
            pinned_account_id: None,
        })
    }

    fn cached_member_auth_for_context(
        &self,
        context: &AccountPoolSelectionContext,
    ) -> Option<(String, CodexAuth)> {
        let assignment_key = self.assignment_key(context.bucket, &context.affinity_key);
        let cache_hint = self.cache_hint_for_context(context, &assignment_key);
        for account_id in self.select_member_ids(context, cache_hint) {
            let Some(member) = self.member(&account_id) else {
                continue;
            };
            let Some(auth) = member.manager.auth_cached_unpooled() else {
                continue;
            };
            if !auth.is_chatgpt_auth() {
                continue;
            }
            return Some((member.account_id.clone(), auth));
        }
        None
    }

    fn status(&self) -> AccountPoolStatus {
        let active_account_id = self.active_account_id();
        let members = self
            .members
            .iter()
            .map(|member| {
                let auth = member.manager.auth_cached_unpooled();
                let state = self.member_state(&member.account_id);
                AccountPoolMemberStatus {
                    account_id: member.account_id.clone(),
                    email: auth.as_ref().and_then(CodexAuth::get_account_email),
                    plan_type: auth.as_ref().and_then(CodexAuth::account_plan_type),
                    active: active_account_id.as_deref() == Some(member.account_id.as_str()),
                    unavailable_reason: state
                        .as_ref()
                        .and_then(|state| state.last_error.clone())
                        .or_else(|| match auth.as_ref() {
                            None => Some("missing credentials".to_string()),
                            Some(auth) if !auth.is_chatgpt_auth() => {
                                Some("account pool members must use ChatGPT auth".to_string())
                            }
                            Some(_) => None,
                        }),
                    regular_remaining: state
                        .as_ref()
                        .and_then(|state| state.usage.as_ref())
                        .and_then(|usage| usage.regular_remaining),
                    spark_remaining: state
                        .as_ref()
                        .and_then(|state| state.usage.as_ref())
                        .and_then(|usage| usage.spark_remaining),
                    credits: state
                        .as_ref()
                        .and_then(|state| state.usage.as_ref())
                        .and_then(|usage| usage.credits.clone()),
                    rate_limit_reset_credit_count: state
                        .as_ref()
                        .and_then(|state| state.usage.as_ref())
                        .and_then(|usage| usage.rate_limit_reset_credit_count),
                    last_error: state.and_then(|state| state.last_error),
                }
            })
            .collect();
        AccountPoolStatus {
            pool_id: self.pool_id.clone(),
            policy: self.definition.policy,
            active_account_id,
            members,
        }
    }

    fn mark_exhausted(&self, account_id: &str, bucket: AccountPoolUsageBucket, error: String) {
        if let Ok(mut guard) = self.state.write() {
            let state = guard.entry(account_id.to_string()).or_default();
            match bucket {
                AccountPoolUsageBucket::Regular => state.regular_exhausted = true,
                AccountPoolUsageBucket::Spark => state.spark_exhausted = true,
            }
            state.last_error = Some(error);
        }
    }

    fn set_last_error(&self, account_id: &str, error: String) {
        if let Ok(mut guard) = self.state.write() {
            guard.entry(account_id.to_string()).or_default().last_error = Some(error);
        }
    }

    fn clear_last_error(&self, account_id: &str) {
        if let Ok(mut guard) = self.state.write()
            && let Some(state) = guard.get_mut(account_id)
        {
            state.last_error = None;
        }
    }

    fn member(&self, account_id: &str) -> Option<&AccountPoolMember> {
        self.members
            .iter()
            .find(|member| member.account_id == account_id)
    }

    fn assignment_key(
        &self,
        bucket: AccountPoolUsageBucket,
        affinity_key: &str,
    ) -> AccountPoolAssignmentKey {
        AccountPoolAssignmentKey::new(self.pool_id.clone(), bucket, affinity_key.to_string())
    }

    fn assignment(&self, key: &AccountPoolAssignmentKey) -> Option<AccountPoolAssignment> {
        self.assignments
            .read()
            .ok()
            .and_then(|guard| guard.get(key).cloned())
    }

    fn assignment_cache_hint(
        &self,
        key: &AccountPoolAssignmentKey,
    ) -> Option<AccountPoolCacheHint> {
        self.assignment(key)
            .and_then(|assignment| assignment.cache_hint)
    }

    fn cache_hint_for_context(
        &self,
        context: &AccountPoolSelectionContext,
        assignment_key: &AccountPoolAssignmentKey,
    ) -> Option<AccountPoolCacheHint> {
        if matches!(context.operation_kind, AccountPoolOperationKind::Compaction) {
            return Some(AccountPoolCacheHint::default());
        }
        context
            .cache_hint
            .or_else(|| self.assignment_cache_hint(assignment_key))
    }

    fn set_assignment(
        &self,
        key: AccountPoolAssignmentKey,
        account_id: String,
        cache_hint: Option<AccountPoolCacheHint>,
    ) {
        if let Ok(mut guard) = self.assignments.write() {
            guard.insert(
                key.clone(),
                AccountPoolAssignment {
                    account_id,
                    cache_hint,
                },
            );
        }
        if let Ok(mut guard) = self.last_assignment_key.write() {
            *guard = Some(key);
        }
    }

    fn record_cache_hint(
        &self,
        bucket: AccountPoolUsageBucket,
        affinity_key: String,
        cache_hint: AccountPoolCacheHint,
    ) {
        let key = self.assignment_key(bucket, &affinity_key);
        if let Ok(mut guard) = self.assignments.write()
            && let Some(assignment) = guard.get_mut(&key)
        {
            assignment.cache_hint = Some(cache_hint);
        }
    }

    fn member_state(&self, account_id: &str) -> Option<MemberRuntimeState> {
        self.state
            .read()
            .ok()
            .and_then(|guard| guard.get(account_id).cloned())
    }

    fn active_account_id(&self) -> Option<String> {
        let key = self
            .last_assignment_key
            .read()
            .ok()
            .and_then(|guard| guard.clone())?;
        self.assignment(&key)
            .map(|assignment| assignment.account_id)
    }

    fn is_exhausted(&self, account_id: &str, bucket: AccountPoolUsageBucket) -> bool {
        self.member_state(account_id)
            .is_some_and(|state| match bucket {
                AccountPoolUsageBucket::Regular => state.regular_exhausted,
                AccountPoolUsageBucket::Spark => state.spark_exhausted,
            })
    }

    async fn refresh_active_token(&self) -> Result<(), RefreshTokenError> {
        let account_id = self.active_account_id().or_else(|| {
            self.cached_member_auth(AccountPoolUsageBucket::Regular)
                .map(|(account_id, _)| account_id)
        });
        let Some(account_id) = account_id else {
            return Ok(());
        };
        let Some(member_manager) = self
            .members
            .iter()
            .find(|member| member.account_id == account_id)
            .map(|member| Arc::clone(&member.manager))
        else {
            return Ok(());
        };
        member_manager.refresh_token_unpooled().await
    }
}

async fn member_auth(account_id: &str, manager: &Arc<AuthManager>) -> Option<CodexAuth> {
    let refresh_manager = Arc::clone(manager);
    // Dropping a timed-out join handle detaches the refresh. Do not cancel a
    // credential rotation that may already have reached the OAuth authority.
    let refresh = tokio::spawn(async move {
        refresh_manager
            .auth_unpooled_cached_for_account_pool_member()
            .await
    });
    let auth = match tokio::time::timeout(usage::USAGE_REQUEST_TIMEOUT, refresh).await {
        Ok(Ok(auth)) => auth,
        Ok(Err(_)) | Err(_) => manager.auth_cached_unpooled(),
    };
    if auth.is_none() {
        tracing::debug!(account_id, "missing account pool member credentials");
    }
    auth
}

#[cfg(test)]
#[path = "account_pool/account_pool_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "account_pool/capacity_tests.rs"]
mod capacity_tests;
