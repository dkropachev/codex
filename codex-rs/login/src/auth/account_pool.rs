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

use super::manager::AuthManager;

const USAGE_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountPoolBucket {
    Regular,
    Spark,
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
    active_account_id: RwLock<Option<String>>,
    state: RwLock<HashMap<String, MemberRuntimeState>>,
}

#[derive(Debug)]
pub struct AccountPoolManager {
    default_pool: String,
    pools: HashMap<String, AccountPool>,
}

impl AccountPoolManager {
    pub fn from_config(
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
        let pools = config
            .pools
            .into_iter()
            .map(|(pool_id, definition)| {
                let members = definition
                    .accounts
                    .iter()
                    .map(|account_id| AccountPoolMember {
                        account_id: account_id.clone(),
                        manager: Arc::new(AuthManager::new(
                            codex_home.join("accounts").join(account_id),
                            /*enable_codex_api_key_env*/ false,
                            auth_credentials_store_mode,
                            chatgpt_base_url.clone(),
                        )),
                    })
                    .collect();
                (
                    pool_id.clone(),
                    AccountPool {
                        pool_id,
                        definition,
                        members,
                        chatgpt_base_url: chatgpt_base_url.clone(),
                        active_account_id: RwLock::new(None),
                        state: RwLock::new(HashMap::new()),
                    },
                )
            })
            .collect();

        Some(Self {
            default_pool,
            pools,
        })
    }

    pub async fn auth(&self) -> Option<CodexAuth> {
        self.auth_for_bucket(AccountPoolBucket::Regular).await
    }

    pub async fn auth_for_bucket(&self, bucket: AccountPoolBucket) -> Option<CodexAuth> {
        let pool = self.pools.get(&self.default_pool)?;
        pool.refresh_stale_usage_for_load_balance().await;
        pool.select_auth(bucket).await
    }

    pub fn auth_cached(&self) -> Option<CodexAuth> {
        let pool = self.pools.get(&self.default_pool)?;
        pool.select_cached_auth(AccountPoolBucket::Regular)
    }

    pub fn status(&self) -> Option<AccountPoolStatus> {
        self.pools.get(&self.default_pool).map(AccountPool::status)
    }

    pub fn default_pool_id(&self) -> &str {
        &self.default_pool
    }

    pub fn mark_exhausted(&self, account_id: &str, bucket: AccountPoolBucket, error: String) {
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

    pub fn mark_active_exhausted(&self, bucket: AccountPoolBucket, error: String) -> bool {
        let Some(pool) = self.pools.get(&self.default_pool) else {
            return false;
        };
        let Some(account_id) = pool.active_account_id() else {
            return false;
        };
        pool.mark_exhausted(&account_id, bucket, error);
        pool.has_available_member(bucket)
    }

    pub async fn refresh_usage(&self, pool_id: Option<&str>) {
        match pool_id {
            Some(pool_id) => {
                if let Some(pool) = self.pools.get(pool_id) {
                    pool.refresh_usage().await;
                }
            }
            None => {
                for pool in self.pools.values() {
                    pool.refresh_usage().await;
                }
            }
        }
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
    async fn select_auth(&self, bucket: AccountPoolBucket) -> Option<CodexAuth> {
        for member in self.select_members(bucket) {
            let Some(auth) = member_auth(&member.account_id, &member.manager).await else {
                self.set_last_error(&member.account_id, "missing credentials".to_string());
                continue;
            };
            if !auth.is_chatgpt_auth() {
                self.set_last_error(
                    &member.account_id,
                    "account pool members must use ChatGPT auth".to_string(),
                );
                continue;
            }
            self.set_active_account_id(member.account_id.clone());
            return Some(auth);
        }
        None
    }

    fn select_cached_auth(&self, bucket: AccountPoolBucket) -> Option<CodexAuth> {
        for member in self.select_members(bucket) {
            let Some(auth) = member.manager.auth_cached_unpooled() else {
                continue;
            };
            if !auth.is_chatgpt_auth() {
                continue;
            }
            self.set_active_account_id(member.account_id.clone());
            return Some(auth);
        }
        None
    }

    fn select_members(&self, bucket: AccountPoolBucket) -> Vec<&AccountPoolMember> {
        let mut members: Vec<_> = self
            .members
            .iter()
            .filter(|member| !self.is_exhausted(&member.account_id, bucket))
            .collect();
        if self.definition.policy == AccountPoolPolicyToml::LoadBalance {
            members.sort_by_key(|member| {
                std::cmp::Reverse(self.remaining(&member.account_id, bucket))
            });
        }
        members
    }

    async fn refresh_stale_usage_for_load_balance(&self) {
        if self.definition.policy != AccountPoolPolicyToml::LoadBalance {
            return;
        }
        if !self.has_stale_usage() {
            return;
        }
        self.refresh_usage().await;
    }

    async fn refresh_usage(&self) {
        let Some(base_url) = self.chatgpt_base_url.as_deref() else {
            for member in &self.members {
                self.set_last_error(
                    &member.account_id,
                    "cannot refresh usage without ChatGPT base URL".to_string(),
                );
            }
            return;
        };

        let mut handles = Vec::with_capacity(self.members.len());
        for member in &self.members {
            handles.push(tokio::spawn(refresh_member_usage(
                member.account_id.clone(),
                Arc::clone(&member.manager),
                base_url.to_string(),
            )));
        }

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
                }
                Err(err) => self.set_last_error(&err.account_id, err.message),
            }
        }
    }

    fn has_stale_usage(&self) -> bool {
        self.members.iter().any(|member| {
            self.member_state(&member.account_id)
                .and_then(|state| state.usage)
                .is_none_or(|usage| usage.last_refreshed.elapsed() > USAGE_REFRESH_INTERVAL)
        })
    }

    fn status(&self) -> AccountPoolStatus {
        let active_account_id = self
            .active_account_id
            .read()
            .ok()
            .and_then(|guard| guard.clone());
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

    fn mark_exhausted(&self, account_id: &str, bucket: AccountPoolBucket, error: String) {
        if let Ok(mut guard) = self.state.write() {
            let state = guard.entry(account_id.to_string()).or_default();
            match bucket {
                AccountPoolBucket::Regular => state.regular_exhausted = true,
                AccountPoolBucket::Spark => state.spark_exhausted = true,
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

    fn set_active_account_id(&self, account_id: String) {
        if let Ok(mut guard) = self.active_account_id.write() {
            *guard = Some(account_id);
        }
    }

    fn member_state(&self, account_id: &str) -> Option<MemberRuntimeState> {
        self.state
            .read()
            .ok()
            .and_then(|guard| guard.get(account_id).cloned())
    }

    fn active_account_id(&self) -> Option<String> {
        self.active_account_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn is_exhausted(&self, account_id: &str, bucket: AccountPoolBucket) -> bool {
        self.member_state(account_id)
            .is_some_and(|state| match bucket {
                AccountPoolBucket::Regular => state.regular_exhausted,
                AccountPoolBucket::Spark => state.spark_exhausted,
            })
    }

    fn remaining(&self, account_id: &str, bucket: AccountPoolBucket) -> u64 {
        self.member_state(account_id)
            .and_then(|state| state.usage)
            .filter(|usage| usage.last_refreshed.elapsed() <= USAGE_REFRESH_INTERVAL)
            .and_then(|usage| match bucket {
                AccountPoolBucket::Regular => usage.regular_remaining,
                AccountPoolBucket::Spark => usage.spark_remaining,
            })
            .unwrap_or(0)
    }

    fn has_available_member(&self, bucket: AccountPoolBucket) -> bool {
        self.members
            .iter()
            .any(|member| !self.is_exhausted(&member.account_id, bucket))
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
    message: String,
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
            message: "missing credentials".to_string(),
        })?;
    match fetch_usage_remaining(&base_url, &auth).await {
        Ok((regular_remaining, spark_remaining)) => Ok((
            MemberRemaining {
                account_id,
                remaining: regular_remaining,
            },
            spark_remaining,
        )),
        Err(message) => Err(MemberRefreshError {
            account_id,
            message,
        }),
    }
}

async fn fetch_usage_remaining(
    base_url: &str,
    auth: &CodexAuth,
) -> Result<(Option<u64>, Option<u64>), String> {
    if !auth.uses_codex_backend() {
        return Err("chatgpt authentication required to refresh usage".to_string());
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
    let token = auth
        .get_token()
        .map_err(|err| format!("failed to read auth token: {err}"))?;
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

    let response = request
        .send()
        .await
        .map_err(|err| format!("failed to fetch codex usage: {err}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "failed to fetch codex usage: {status}; body={body}"
        ));
    }
    let payload: Value = serde_json::from_str(&body)
        .map_err(|err| format!("failed to decode codex usage response: {err}"))?;
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
    let auth = manager.auth_unpooled_cached().await;
    if auth.is_none() {
        tracing::debug!(account_id, "missing account pool member credentials");
    }
    auth
}

fn remaining_from_rate_limit(rate_limit: Option<&Value>) -> Option<u64> {
    let used_percent = rate_limit?
        .get("primary_window")?
        .get("used_percent")
        .and_then(Value::as_f64)?;
    Some((100.0 - used_percent).clamp(0.0, 100.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use std::fs;
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
        );

        let auth = pool.auth().await.expect("pool should select auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
        assert_eq!(
            pool.status().expect("status").active_account_id.as_deref(),
            Some("work-pro")
        );
    }

    #[tokio::test]
    async fn missing_member_does_not_invalidate_pool() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        );

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
    async fn load_balance_selects_largest_fresh_remaining_bucket() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
        );
        pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
        pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

        let regular = pool
            .auth_for_bucket(AccountPoolBucket::Regular)
            .await
            .expect("regular auth");
        assert_eq!(
            regular.get_account_email().as_deref(),
            Some("personal@example.com")
        );

        let spark = pool
            .auth_for_bucket(AccountPoolBucket::Spark)
            .await
            .expect("spark auth");
        assert_eq!(
            spark.get_account_email().as_deref(),
            Some("work@example.com")
        );
    }

    #[tokio::test]
    async fn refresh_usage_requests_pool_members_in_parallel() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
        write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "rate_limit": {
                            "primary_window": {
                                "used_percent": 25.0
                            }
                        }
                    }))
                    .set_delay(Duration::from_millis(1000)),
            )
            .expect(2)
            .mount(&server)
            .await;

        let pool = pool_manager_with_base_url(
            codex_home.path(),
            AccountPoolPolicyToml::LoadBalance,
            vec!["work-pro", "personal-pro"],
            Some(server.uri()),
        );

        let started = Instant::now();
        pool.refresh_usage(/*pool_id*/ None).await;
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "usage refresh should run member requests in parallel"
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
        );
        pool.mark_exhausted(
            "work-pro",
            AccountPoolBucket::Regular,
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
        );
        pool.mark_exhausted(
            "work-pro",
            AccountPoolBucket::Regular,
            "usage limit reached".to_string(),
        );
        pool.set_usage_for_testing("work-pro", Some(100), None, Instant::now());

        let auth = pool
            .auth()
            .await
            .expect("pool should select refreshed auth");
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("work@example.com")
        );
    }

    #[test]
    fn mark_active_exhausted_reports_whether_bucket_has_fallback() {
        let codex_home = tempfile::tempdir().expect("tempdir");
        let pool = pool_manager(
            codex_home.path(),
            AccountPoolPolicyToml::Drain,
            vec!["work-pro", "personal-pro"],
        );
        let account_pool = pool.pools.get("codex-pro").expect("pool");
        account_pool.set_active_account_id("work-pro".to_string());

        assert!(pool.mark_active_exhausted(
            AccountPoolBucket::Regular,
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
        assert_eq!(remaining_from_rate_limit(Some(&json!({}))), None);
    }

    fn pool_manager(
        codex_home: &Path,
        policy: AccountPoolPolicyToml,
        accounts: Vec<&str>,
    ) -> AccountPoolManager {
        pool_manager_with_base_url(codex_home, policy, accounts, /*chatgpt_base_url*/ None)
    }

    fn pool_manager_with_base_url(
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
        .expect("account pool should be enabled")
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
