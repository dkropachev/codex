use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;
use std::time::Instant;

use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::types::AuthCredentialsStoreMode;
use codex_protocol::account::PlanType;
use serde_json::Value;

use crate::CodexAuth;

use super::account_pool_selection::AccountPoolAssignmentKey;
use super::account_pool_selection::AccountPoolAuthSelection;
use super::account_pool_selection::AccountPoolCacheHeat;
use super::account_pool_selection::AccountPoolCacheHint;
use super::account_pool_selection::AccountPoolOperationKind;
use super::account_pool_selection::AccountPoolSelectionContext;
use super::account_pool_selection::AccountPoolUsageBucket;
use super::account_pool_selection::DEFAULT_ACCOUNT_POOL_AFFINITY_KEY;
use super::manager::AuthManager;
use super::manager::RefreshTokenError;

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

#[derive(Clone, Debug)]
struct MemberUsage {
    regular_remaining: Option<u64>,
    spark_remaining: Option<u64>,
    last_refreshed: Instant,
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
    pub async fn from_config(
        codex_home: &Path,
        config: AccountPoolToml,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<String>,
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
                members.push(AccountPoolMember {
                    account_id: account_id.clone(),
                    manager: Arc::new(
                        AuthManager::new(
                            codex_home.join("accounts").join(account_id),
                            /*enable_codex_api_key_env*/ false,
                            auth_credentials_store_mode,
                            chatgpt_base_url.clone(),
                        )
                        .await,
                    ),
                });
            }
            pools.insert(
                pool_id.clone(),
                AccountPool {
                    pool_id,
                    definition,
                    members,
                    chatgpt_base_url: chatgpt_base_url.clone(),
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
        pool.refresh_stale_usage_for_load_balance().await;
        pool.select_auth(context).await
    }

    pub fn auth_cached(&self) -> Option<CodexAuth> {
        let pool = self.pools.get(&self.default_pool)?;
        pool.select_cached_auth(AccountPoolUsageBucket::Regular)
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

    fn select_cached_auth(&self, bucket: AccountPoolUsageBucket) -> Option<CodexAuth> {
        self.cached_member_auth(bucket).map(|(_, auth)| auth)
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

    fn select_member_ids(
        &self,
        context: &AccountPoolSelectionContext,
        cache_hint: Option<AccountPoolCacheHint>,
    ) -> Vec<String> {
        let bucket = context.bucket;
        let mut account_ids: Vec<_> = self
            .members
            .iter()
            .filter(|member| !self.is_exhausted(&member.account_id, bucket))
            .map(|member| member.account_id.clone())
            .collect();
        if let Some(pinned_account_id) = context.pinned_account_id.as_deref() {
            return account_ids
                .into_iter()
                .filter(|account_id| account_id == pinned_account_id)
                .collect();
        }
        let assignment_key = self.assignment_key(bucket, &context.affinity_key);
        if let Some(assignment) = self.assignment(&assignment_key)
            && let Some(active_index) = account_ids
                .iter()
                .position(|account_id| account_id == &assignment.account_id)
        {
            if self.definition.policy != AccountPoolPolicyToml::LoadBalance {
                let active_account_id = account_ids.remove(active_index);
                account_ids.insert(0, active_account_id);
                return account_ids;
            }
            match cache_hint
                .map(|hint| hint.heat())
                .unwrap_or(AccountPoolCacheHeat::Cold)
            {
                AccountPoolCacheHeat::Cold => {}
                AccountPoolCacheHeat::Warm | AccountPoolCacheHeat::Hot
                    if !self.should_rebalance_assignment(
                        &assignment.account_id,
                        bucket,
                        cache_hint,
                    ) =>
                {
                    let active_account_id = account_ids.remove(active_index);
                    account_ids.insert(0, active_account_id);
                    return account_ids;
                }
                AccountPoolCacheHeat::Warm | AccountPoolCacheHeat::Hot => {}
            }
        }
        if self.definition.policy == AccountPoolPolicyToml::LoadBalance {
            account_ids.sort_by_key(|account_id| {
                std::cmp::Reverse(self.member_score(account_id, bucket, &assignment_key))
            });
        }
        account_ids
    }

    async fn refresh_stale_usage_for_load_balance(&self) {
        if self.definition.policy != AccountPoolPolicyToml::LoadBalance {
            return;
        }
        if !self.has_stale_usage() {
            return;
        }
        if !self.should_start_usage_refresh_attempt() {
            return;
        }
        self.refresh_usage().await;
    }

    async fn refresh_usage(&self) -> AccountPoolUsageRefreshPoolReport {
        self.record_usage_refresh_attempt();
        let Some(base_url) = self.chatgpt_base_url.as_deref() else {
            for member in &self.members {
                self.set_last_error(
                    &member.account_id,
                    "cannot refresh usage without ChatGPT base URL".to_string(),
                );
            }
            return AccountPoolUsageRefreshPoolReport {
                pool_id: self.pool_id.clone(),
                member_count: self.members.len(),
                refreshed_count: 0,
                usable_credentials_count: 0,
                problems: self
                    .members
                    .iter()
                    .map(|member| AccountPoolUsageRefreshProblem {
                        account_id: member.account_id.clone(),
                        message: "cannot refresh usage without ChatGPT base URL".to_string(),
                    })
                    .collect(),
            };
        };

        let mut handles = Vec::with_capacity(self.members.len());
        for member in &self.members {
            handles.push(tokio::spawn(refresh_member_usage(
                member.account_id.clone(),
                Arc::clone(&member.manager),
                base_url.to_string(),
            )));
        }

        let mut outcomes = HashMap::new();
        for handle in handles {
            let result = match handle.await {
                Ok(result) => result,
                Err(err) => {
                    tracing::debug!("failed to join account pool usage refresh task: {err}");
                    continue;
                }
            };
            match result {
                Ok((regular_remaining, spark_remaining)) => {
                    self.set_usage(
                        &regular_remaining.account_id,
                        regular_remaining.remaining,
                        spark_remaining,
                        Instant::now(),
                    );
                    self.clear_last_error(&regular_remaining.account_id);
                    outcomes.insert(
                        regular_remaining.account_id.clone(),
                        MemberRefreshOutcome::Refreshed,
                    );
                }
                Err(err) => {
                    self.set_last_error(&err.account_id, err.message.clone());
                    outcomes.insert(err.account_id, MemberRefreshOutcome::Problem(err.kind));
                }
            }
        }

        let mut refreshed_count = 0;
        let mut usable_credentials_count = 0;
        let mut problems = Vec::new();
        for member in &self.members {
            match outcomes.get(&member.account_id) {
                Some(MemberRefreshOutcome::Refreshed) => {
                    refreshed_count += 1;
                    usable_credentials_count += 1;
                }
                Some(MemberRefreshOutcome::Problem(kind)) => {
                    if kind.has_usable_credentials() {
                        usable_credentials_count += 1;
                    }
                    problems.push(AccountPoolUsageRefreshProblem {
                        account_id: member.account_id.clone(),
                        message: kind.message().to_string(),
                    });
                }
                None => problems.push(AccountPoolUsageRefreshProblem {
                    account_id: member.account_id.clone(),
                    message: "usage refresh did not complete".to_string(),
                }),
            }
        }

        AccountPoolUsageRefreshPoolReport {
            pool_id: self.pool_id.clone(),
            member_count: self.members.len(),
            refreshed_count,
            usable_credentials_count,
            problems,
        }
    }

    fn has_stale_usage(&self) -> bool {
        self.members.iter().any(|member| {
            self.member_state(&member.account_id)
                .and_then(|state| state.usage)
                .is_none_or(|usage| usage.last_refreshed.elapsed() > USAGE_REFRESH_INTERVAL)
        })
    }

    fn should_start_usage_refresh_attempt(&self) -> bool {
        let Ok(mut guard) = self.last_usage_refresh_attempt.write() else {
            return true;
        };
        if guard.is_none_or(|last_attempt| last_attempt.elapsed() > USAGE_REFRESH_INTERVAL) {
            *guard = Some(Instant::now());
            return true;
        }
        false
    }

    fn record_usage_refresh_attempt(&self) {
        if let Ok(mut guard) = self.last_usage_refresh_attempt.write() {
            *guard = Some(Instant::now());
        }
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

    fn set_usage(
        &self,
        account_id: &str,
        regular_remaining: Option<u64>,
        spark_remaining: Option<u64>,
        last_refreshed: Instant,
    ) {
        if let Ok(mut guard) = self.state.write() {
            let state = guard.entry(account_id.to_string()).or_default();
            if regular_remaining.is_some_and(|remaining| remaining > 0) {
                state.regular_exhausted = false;
            }
            if spark_remaining.is_some_and(|remaining| remaining > 0) {
                state.spark_exhausted = false;
            }
            state.usage = Some(MemberUsage {
                regular_remaining,
                spark_remaining,
                last_refreshed,
            });
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

    fn member_score(
        &self,
        account_id: &str,
        bucket: AccountPoolUsageBucket,
        assignment_key: &AccountPoolAssignmentKey,
    ) -> i64 {
        let (remaining, unknown_usage_penalty) = match self.fresh_remaining(account_id, bucket) {
            Some(remaining) => (remaining as i64, 0),
            None => (0, UNKNOWN_USAGE_PENALTY),
        };
        let active_affinity_penalty = self.active_assignment_count(account_id, assignment_key)
            as i64
            * ACTIVE_AFFINITY_PENALTY;
        remaining - unknown_usage_penalty - active_affinity_penalty
    }

    fn active_assignment_count(
        &self,
        account_id: &str,
        current_key: &AccountPoolAssignmentKey,
    ) -> usize {
        let Ok(guard) = self.assignments.read() else {
            return 0;
        };
        guard
            .iter()
            .filter(|(key, assignment)| key != &current_key && assignment.account_id == account_id)
            .count()
    }

    fn fresh_remaining(&self, account_id: &str, bucket: AccountPoolUsageBucket) -> Option<u64> {
        self.member_state(account_id)
            .and_then(|state| state.usage)
            .filter(|usage| usage.last_refreshed.elapsed() <= USAGE_REFRESH_INTERVAL)
            .and_then(|usage| match bucket {
                AccountPoolUsageBucket::Regular => usage.regular_remaining,
                AccountPoolUsageBucket::Spark => usage.spark_remaining,
            })
    }

    fn should_rebalance_assignment(
        &self,
        active_account_id: &str,
        bucket: AccountPoolUsageBucket,
        cache_hint: Option<AccountPoolCacheHint>,
    ) -> bool {
        let Some(active_remaining) = self.fresh_remaining(active_account_id, bucket) else {
            return false;
        };
        let (low_remaining, material_advantage) = match cache_hint
            .map(|hint| hint.heat())
            .unwrap_or(AccountPoolCacheHeat::Cold)
        {
            AccountPoolCacheHeat::Cold => return true,
            AccountPoolCacheHeat::Warm => (
                WARM_REBALANCE_LOW_REMAINING,
                WARM_REBALANCE_MATERIAL_ADVANTAGE,
            ),
            AccountPoolCacheHeat::Hot => (
                HOT_REBALANCE_LOW_REMAINING,
                HOT_REBALANCE_MATERIAL_ADVANTAGE,
            ),
        };
        if active_remaining > low_remaining {
            return false;
        }
        self.members
            .iter()
            .filter(|member| member.account_id != active_account_id)
            .filter(|member| !self.is_exhausted(&member.account_id, bucket))
            .filter_map(|member| self.fresh_remaining(&member.account_id, bucket))
            .max()
            .is_some_and(|best_remaining| {
                best_remaining >= active_remaining.saturating_add(material_advantage)
            })
    }

    fn has_available_member(&self, bucket: AccountPoolUsageBucket) -> bool {
        self.members.iter().any(|member| {
            !self.is_exhausted(&member.account_id, bucket)
                && member
                    .manager
                    .auth_cached_unpooled()
                    .is_some_and(|auth| auth.is_chatgpt_auth())
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

#[derive(Debug, PartialEq, Eq)]
struct MemberRemaining {
    account_id: String,
    remaining: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct MemberRefreshError {
    account_id: String,
    kind: MemberRefreshErrorKind,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemberRefreshOutcome {
    Refreshed,
    Problem(MemberRefreshErrorKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemberRefreshErrorKind {
    MissingCredentials,
    InvalidCredentials,
    UsageRefreshFailed {
        message: String,
        has_usable_credentials: bool,
    },
}

impl MemberRefreshErrorKind {
    fn message(&self) -> &str {
        match self {
            Self::MissingCredentials => "missing credentials",
            Self::InvalidCredentials => "invalid credentials",
            Self::UsageRefreshFailed { message, .. } => message,
        }
    }

    fn has_usable_credentials(&self) -> bool {
        match self {
            Self::MissingCredentials | Self::InvalidCredentials => false,
            Self::UsageRefreshFailed {
                has_usable_credentials,
                ..
            } => *has_usable_credentials,
        }
    }
}

async fn refresh_member_usage(
    account_id: String,
    manager: Arc<AuthManager>,
    base_url: String,
) -> Result<(MemberRemaining, Option<u64>), MemberRefreshError> {
    let auth = member_auth(&account_id, &manager)
        .await
        .ok_or_else(|| MemberRefreshError {
            account_id: account_id.clone(),
            kind: MemberRefreshErrorKind::MissingCredentials,
            message: "missing credentials".to_string(),
        })?;
    if !auth.is_chatgpt_auth() {
        return Err(MemberRefreshError {
            account_id,
            kind: MemberRefreshErrorKind::InvalidCredentials,
            message: "invalid credentials".to_string(),
        });
    }
    match fetch_usage_remaining(&base_url, &auth).await {
        Ok((regular_remaining, spark_remaining)) => Ok((
            MemberRemaining {
                account_id,
                remaining: regular_remaining,
            },
            spark_remaining,
        )),
        Err(failure) => Err(MemberRefreshError {
            account_id,
            kind: MemberRefreshErrorKind::UsageRefreshFailed {
                message: failure.message.clone(),
                has_usable_credentials: failure.has_usable_credentials,
            },
            message: failure.message,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageRefreshFailure {
    message: String,
    has_usable_credentials: bool,
}

impl UsageRefreshFailure {
    fn with_usable_credentials(message: String) -> Self {
        Self {
            message,
            has_usable_credentials: true,
        }
    }

    fn auth_rejected(message: String) -> Self {
        Self {
            message,
            has_usable_credentials: false,
        }
    }
}

async fn fetch_usage_remaining(
    base_url: &str,
    auth: &CodexAuth,
) -> Result<(Option<u64>, Option<u64>), UsageRefreshFailure> {
    if !auth.uses_codex_backend() {
        return Err(UsageRefreshFailure::auth_rejected(
            "chatgpt authentication required to refresh usage".to_string(),
        ));
    }

    let mut base_url = base_url.trim_end_matches('/').to_string();
    if (base_url.starts_with("https://chatgpt.com")
        || base_url.starts_with("https://chat.openai.com"))
        && !base_url.contains("/backend-api")
    {
        base_url = format!("{base_url}/backend-api");
    }
    let path = if base_url.contains("/backend-api") {
        "wham/usage"
    } else {
        "api/codex/usage"
    };
    let url = format!("{base_url}/{path}");
    let token = auth.get_token().map_err(|err| {
        UsageRefreshFailure::auth_rejected(format!("failed to read auth token: {err}"))
    })?;
    let mut request = reqwest::Client::new()
        .get(&url)
        .header(reqwest::header::USER_AGENT, "codex-cli")
        .bearer_auth(token);
    if let Some(account_id) = auth.get_account_id() {
        request = request.header("ChatGPT-Account-ID", account_id);
    }
    if auth.is_fedramp_account() {
        request = request.header("X-OpenAI-Fedramp", "true");
    }

    let response = request.send().await.map_err(|err| {
        UsageRefreshFailure::with_usable_credentials(format!("failed to fetch codex usage: {err}"))
    })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let message = format!("failed to fetch codex usage: {status}; body={body}");
        if matches!(
            status,
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(UsageRefreshFailure::auth_rejected(message));
        }
        return Err(UsageRefreshFailure::with_usable_credentials(message));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|err| {
        UsageRefreshFailure::with_usable_credentials(format!(
            "failed to decode codex usage response: {err}"
        ))
    })?;
    let regular_remaining = remaining_from_rate_limit(payload.get("rate_limit"));
    let spark_remaining = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array)
        .and_then(|limits| {
            limits
                .iter()
                .filter(|limit| {
                    limit
                        .get("metered_feature")
                        .and_then(Value::as_str)
                        .is_some_and(|limit_id| limit_id != "codex")
                })
                .filter_map(|limit| remaining_from_rate_limit(limit.get("rate_limit")))
                .max()
        });
    Ok((regular_remaining, spark_remaining))
}

async fn member_auth(account_id: &str, manager: &AuthManager) -> Option<CodexAuth> {
    let auth = manager.auth_unpooled_cached_for_account_pool_member().await;
    if auth.is_none() {
        tracing::debug!(account_id, "missing account pool member credentials");
    }
    auth
}

fn remaining_from_rate_limit(rate_limit: Option<&Value>) -> Option<u64> {
    let rate_limit = rate_limit?;
    ["primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|window| remaining_from_window(rate_limit.get(window)))
        .min()
}

fn remaining_from_window(window: Option<&Value>) -> Option<u64> {
    let used_percent = window?.get("used_percent").and_then(Value::as_f64)?;
    Some((100.0 - used_percent).clamp(0.0, 100.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use base64::Engine;
    use chrono::Utc;
    use codex_app_server_protocol::AuthMode;
    use codex_config::config_toml::AccountPoolDefinitionToml;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    #[tokio::test]
    async fn drain_selects_first_available_subscription_account() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;

        let auth = pool.auth().await.expect("pool should select auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
        assert_eq!(pool.status(), Some(active_work_pool_status()));
    }

    #[tokio::test]
    async fn missing_member_does_not_invalidate_pool() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;

        let auth = pool.auth().await.expect("pool should select fallback auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("personal@example.com")
        );
        let status = pool.status().expect("status");
        assert_eq!(
            status
                .members
                .iter()
                .find(|member| member.account_id == "work-pro")
                .and_then(|member| member.unavailable_reason.as_deref()),
            Some("missing credentials")
        );
    }

    #[tokio::test]
    async fn cached_auth_does_not_activate_pool_member() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;

        let auth = pool.auth_cached().expect("pool should select cached auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
        assert_eq!(
            pool.status().expect("status").active_account_id.as_deref(),
            None
        );
    }

    #[tokio::test]
    async fn explicit_cached_activation_selects_pool_member() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;

        let auth = pool
            .activate_cached_auth()
            .expect("pool should activate cached auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
        assert_eq!(pool.status(), Some(active_work_pool_status()));
    }

    #[tokio::test]
    async fn load_balance_selects_largest_fresh_remaining_bucket() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let regular_pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        regular_pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
        regular_pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

        let regular = regular_pool
            .auth_for_bucket(AccountPoolUsageBucket::Regular)
            .await
            .expect("regular auth");
        assert_eq!(
            regular.get_account_email().as_deref(),
            Some("personal@example.com")
        );

        let spark_pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        spark_pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
        spark_pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

        let spark = spark_pool
            .auth_for_bucket(AccountPoolUsageBucket::Spark)
            .await
            .expect("spark auth");
        assert_eq!(
            spark.get_account_email().as_deref(),
            Some("work@example.com")
        );
    }

    #[tokio::test]
    async fn load_balance_keeps_active_member_when_available() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        pool.set_usage_for_testing("work-pro", Some(50), Some(50), Instant::now());
        pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());

        let account_pool = pool.pools.get("codex-pro").expect("pool");
        let affinity_key = "thread-hot";
        account_pool.set_assignment(
            account_pool.assignment_key(AccountPoolUsageBucket::Regular, affinity_key),
            "work-pro".to_string(),
            Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }),
        );

        let auth = pool
            .auth_for_context(AccountPoolSelectionContext {
                bucket: AccountPoolUsageBucket::Regular,
                affinity_key: affinity_key.to_string(),
                operation_kind: AccountPoolOperationKind::Stream,
                cache_hint: Some(AccountPoolCacheHint {
                    input_tokens: 20_000,
                    cached_input_tokens: 12_000,
                }),
                pinned_account_id: None,
            })
            .await
            .expect("regular auth");
        assert_eq!(
            auth.auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
    }

    #[tokio::test]
    async fn load_balance_rebalances_warm_affinity_when_another_member_is_healthier() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        pool.set_usage_for_testing("work-pro", Some(5), Some(5), Instant::now());
        pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());
        let account_pool = pool.pools.get("codex-pro").expect("pool");
        let affinity_key = "thread-warm";
        let cache_hint = AccountPoolCacheHint {
            input_tokens: 10_000,
            cached_input_tokens: 3_000,
        };
        account_pool.set_assignment(
            account_pool.assignment_key(AccountPoolUsageBucket::Regular, affinity_key),
            "work-pro".to_string(),
            Some(cache_hint),
        );

        let auth = pool
            .auth_for_context(AccountPoolSelectionContext {
                bucket: AccountPoolUsageBucket::Regular,
                affinity_key: affinity_key.to_string(),
                operation_kind: AccountPoolOperationKind::Stream,
                cache_hint: Some(cache_hint),
                pinned_account_id: None,
            })
            .await
            .expect("regular auth");

        assert_eq!(
            auth.auth.get_account_email().as_deref(),
            Some("personal@example.com")
        );
    }

    #[tokio::test]
    async fn load_balance_stores_assignments_by_affinity_and_usage_bucket() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        pool.set_usage_for_testing("work-pro", Some(90), Some(1), Instant::now());
        pool.set_usage_for_testing("personal-pro", Some(1), Some(90), Instant::now());

        let regular = pool
            .auth_for_context(AccountPoolSelectionContext {
                bucket: AccountPoolUsageBucket::Regular,
                affinity_key: "thread-a".to_string(),
                operation_kind: AccountPoolOperationKind::Stream,
                cache_hint: None,
                pinned_account_id: None,
            })
            .await
            .expect("regular auth");
        let spark = pool
            .auth_for_context(AccountPoolSelectionContext {
                bucket: AccountPoolUsageBucket::Spark,
                affinity_key: "thread-a".to_string(),
                operation_kind: AccountPoolOperationKind::Stream,
                cache_hint: None,
                pinned_account_id: None,
            })
            .await
            .expect("spark auth");

        assert_eq!(
            regular.auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
        assert_eq!(
            spark.auth.get_account_email().as_deref(),
            Some("personal@example.com")
        );
        let account_pool = pool.pools.get("codex-pro").expect("pool");
        assert_eq!(
            account_pool
                .assignment(
                    &account_pool.assignment_key(AccountPoolUsageBucket::Regular, "thread-a")
                )
                .expect("regular assignment")
                .account_id,
            "work-pro"
        );
        assert_eq!(
            account_pool
                .assignment(&account_pool.assignment_key(AccountPoolUsageBucket::Spark, "thread-a"))
                .expect("spark assignment")
                .account_id,
            "personal-pro"
        );
    }

    #[tokio::test]
    async fn load_balance_throttles_stale_usage_refresh_attempts() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(500))
            .expect(2)
            .mount(&server)
            .await;

        let pool = pool_manager_with_base_url(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
            Some(server.uri()),
        )
        .await;

        let first_auth = pool
            .auth_for_bucket(AccountPoolUsageBucket::Regular)
            .await
            .expect("first auth should still select credentials");
        let second_auth = pool
            .auth_for_bucket(AccountPoolUsageBucket::Regular)
            .await
            .expect("second auth should still select credentials");

        assert_eq!(
            first_auth.get_account_email(),
            second_auth.get_account_email()
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn refresh_usage_requests_pool_members_in_parallel() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let server = MockServer::start().await;
        let request_count = Arc::new(AtomicUsize::new(0));
        let responder_request_count = Arc::clone(&request_count);
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(move |_request: &wiremock::Request| {
                responder_request_count.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "rate_limit": {
                            "primary_window": {
                                "used_percent": 25.0
                            }
                        }
                    }))
                    .set_delay(Duration::from_millis(/*millis*/ 2_000))
            })
            .expect(2)
            .mount(&server)
            .await;

        let pool = pool_manager_with_base_url(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
            Some(server.uri()),
        )
        .await;

        let observed_request_count = Arc::clone(&request_count);
        let ((), request_count_while_first_response_delayed) =
            tokio::join!(pool.refresh_usage(/*pool_id*/ None), async move {
                tokio::time::sleep(Duration::from_millis(/*millis*/ 1_000)).await;
                observed_request_count.load(Ordering::SeqCst)
            });
        assert_eq!(
            request_count_while_first_response_delayed, 2,
            "usage refresh should start all member requests before the first response returns"
        );

        assert_eq!(
            pool.status(),
            Some(AccountPoolStatus {
                pool_id: "codex-pro".to_string(),
                policy: AccountPoolPolicyToml::LoadBalance,
                active_account_id: None,
                members: vec![
                    AccountPoolMemberStatus {
                        account_id: "work-pro".to_string(),
                        email: Some("work@example.com".to_string()),
                        plan_type: Some(PlanType::Pro),
                        active: false,
                        unavailable_reason: None,
                        regular_remaining: Some(75),
                        spark_remaining: None,
                        last_error: None,
                    },
                    AccountPoolMemberStatus {
                        account_id: "personal-pro".to_string(),
                        email: Some("personal@example.com".to_string()),
                        plan_type: Some(PlanType::Pro),
                        active: false,
                        unavailable_reason: None,
                        regular_remaining: Some(75),
                        spark_remaining: None,
                        last_error: None,
                    },
                ],
            })
        );
    }

    #[tokio::test]
    async fn exhausted_member_is_skipped_for_that_bucket() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        pool.mark_exhausted(
            "work-pro",
            AccountPoolUsageBucket::Regular,
            "usage limit reached".to_string(),
        );

        let auth = pool
            .auth()
            .await
            .expect("pool should select non-exhausted auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("personal@example.com")
        );
    }

    #[tokio::test]
    async fn refreshed_positive_usage_makes_exhausted_member_available_again() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        pool.mark_exhausted(
            "work-pro",
            AccountPoolUsageBucket::Regular,
            "usage limit reached".to_string(),
        );
        pool.set_usage_for_testing(
            "work-pro",
            Some(100),
            /*spark_remaining*/ None,
            Instant::now(),
        );

        let auth = pool
            .auth()
            .await
            .expect("pool should select refreshed auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
    }

    #[tokio::test]
    async fn mark_active_exhausted_reports_whether_bucket_has_usable_fallback() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        )
        .await;
        let account_pool = pool.pools.get("codex-pro").expect("pool");
        account_pool.set_assignment(
            account_pool.assignment_key(
                AccountPoolUsageBucket::Regular,
                DEFAULT_ACCOUNT_POOL_AFFINITY_KEY,
            ),
            "work-pro".to_string(),
            /*cache_hint*/ None,
        );

        assert!(!pool.mark_active_exhausted(
            AccountPoolUsageBucket::Regular,
            "usage limit reached".to_string()
        ));
        assert_eq!(
            pool.status(),
            Some(AccountPoolStatus {
                pool_id: "codex-pro".to_string(),
                policy: AccountPoolPolicyToml::Drain,
                active_account_id: Some("work-pro".to_string()),
                members: vec![
                    AccountPoolMemberStatus {
                        account_id: "work-pro".to_string(),
                        email: None,
                        plan_type: None,
                        active: true,
                        unavailable_reason: Some("usage limit reached".to_string()),
                        regular_remaining: None,
                        spark_remaining: None,
                        last_error: Some("usage limit reached".to_string()),
                    },
                    AccountPoolMemberStatus {
                        account_id: "personal-pro".to_string(),
                        email: None,
                        plan_type: None,
                        active: false,
                        unavailable_reason: Some("missing credentials".to_string()),
                        regular_remaining: None,
                        spark_remaining: None,
                        last_error: None,
                    },
                ],
            })
        );
    }

    #[test]
    fn remaining_from_rate_limit_maps_used_percent_to_remaining_percent() {
        assert_eq!(
            remaining_from_rate_limit(Some(&json!({
                "primary_window": { "used_percent": 12.4 }
            }))),
            Some(88)
        );
        assert_eq!(
            remaining_from_rate_limit(Some(&json!({
                "primary_window": { "used_percent": 150.0 }
            }))),
            Some(0)
        );
        assert_eq!(
            remaining_from_rate_limit(Some(&json!({
                "primary_window": { "used_percent": -7.0 }
            }))),
            Some(100)
        );
        assert_eq!(
            remaining_from_rate_limit(Some(&json!({
                "primary_window": { "used_percent": 0.0 },
                "secondary_window": { "used_percent": 14.0 }
            }))),
            Some(86)
        );
        assert_eq!(remaining_from_rate_limit(Some(&json!({}))), None);
    }

    async fn pool_manager(
        codex_home: &Path,
        policy: AccountPoolPolicyToml,
        accounts: Vec<&str>,
    ) -> AccountPoolManager {
        pool_manager_with_base_url(codex_home, policy, accounts, /*chatgpt_base_url*/ None).await
    }

    async fn pool_manager_with_base_url(
        codex_home: &Path,
        policy: AccountPoolPolicyToml,
        accounts: Vec<&str>,
        chatgpt_base_url: Option<String>,
    ) -> AccountPoolManager {
        AccountPoolManager::from_config(
            codex_home,
            AccountPoolToml {
                enabled: true,
                default_pool: Some("codex-pro".to_string()),
                pools: [(
                    "codex-pro".to_string(),
                    AccountPoolDefinitionToml {
                        provider: "openai".to_string(),
                        policy,
                        accounts: accounts.into_iter().map(str::to_string).collect(),
                    },
                )]
                .into(),
            },
            AuthCredentialsStoreMode::File,
            chatgpt_base_url,
        )
        .await
        .expect("account pool should be enabled")
    }

    fn active_work_pool_status() -> AccountPoolStatus {
        AccountPoolStatus {
            pool_id: "codex-pro".to_string(),
            policy: AccountPoolPolicyToml::Drain,
            active_account_id: Some("work-pro".to_string()),
            members: vec![
                AccountPoolMemberStatus {
                    account_id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(PlanType::Pro),
                    active: true,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMemberStatus {
                    account_id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(PlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
            ],
        }
    }

    fn write_chatgpt_auth(codex_home: &Path, account_id: &str, email: &str) {
        let account_home = codex_home.join("accounts").join(account_id);
        fs::create_dir_all(&account_home).expect("create account home");
        let jwt = fake_jwt(json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_account_id": account_id,
                "user_id": format!("user-{account_id}")
            }
        }));
        let auth = json!({
            "auth_mode": AuthMode::Chatgpt,
            "tokens": {
                "id_token": jwt,
                "access_token": fake_jwt(json!({ "exp": Utc::now().timestamp() + 3600 })),
                "refresh_token": format!("refresh-{account_id}"),
                "account_id": account_id,
            },
            "last_refresh": Utc::now(),
        });
        fs::write(
            account_home.join("auth.json"),
            serde_json::to_string_pretty(&auth).expect("serialize auth"),
        )
        .expect("write auth");
    }

    fn fake_jwt(payload: serde_json::Value) -> String {
        let header = json!({"alg": "none"});
        let encode = |value: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&value).expect("serialize jwt part"))
        };
        format!("{}.{}.sig", encode(header), encode(payload))
    }
}
