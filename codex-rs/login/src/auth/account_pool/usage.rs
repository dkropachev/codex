//! Passive, bounded account usage refresh. Reading usage never redeems a reset.

use super::*;
use codex_protocol::protocol::CreditsSnapshot;
use serde_json::Value;

pub(super) const USAGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub(super) struct MemberUsage {
    pub(super) regular_remaining: Option<u64>,
    pub(super) spark_remaining: Option<u64>,
    pub(super) last_refreshed: Instant,
    pub(super) ordinary_usage_allowed: Option<bool>,
    pub(super) spark_usage_allowed: Option<bool>,
    pub(super) credits: Option<CreditsSnapshot>,
    pub(super) rate_limit_reset_credit_count: Option<i64>,
    pub(super) spend_control_reached: Option<bool>,
}

impl AccountPool {
    pub(super) async fn refresh_stale_usage(&self) {
        if self.chatgpt_base_url.is_none() {
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

    pub(super) async fn refresh_usage(&self) -> AccountPoolUsageRefreshPoolReport {
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

        let deadline = tokio::time::Instant::now() + USAGE_REQUEST_TIMEOUT;
        let mut handles = Vec::with_capacity(self.members.len());
        for member in &self.members {
            handles.push((
                member.account_id.clone(),
                Arc::clone(&member.manager),
                tokio::spawn(refresh_member_usage(
                    member.account_id.clone(),
                    Arc::clone(&member.manager),
                    base_url.to_string(),
                    self.auth_route_config.clone(),
                )),
            ));
        }

        let mut outcomes = HashMap::new();
        for (account_id, manager, handle) in handles {
            let result = match tokio::time::timeout_at(deadline, handle).await {
                Ok(Ok(result)) => result,
                Err(_) => Err(MemberRefreshError {
                    account_id,
                    kind: MemberRefreshErrorKind::UsageRefreshFailed {
                        message: "usage refresh timed out".to_string(),
                        has_usable_credentials: manager
                            .auth_cached_unpooled()
                            .is_some_and(|auth| auth.is_chatgpt_auth()),
                    },
                    message: "usage refresh timed out".to_string(),
                }),
                Ok(Err(err)) => {
                    tracing::debug!("failed to join account pool usage refresh task: {err}");
                    continue;
                }
            };
            match result {
                Ok((account_id, usage)) => {
                    self.apply_usage(&account_id, usage);
                    self.clear_last_error(&account_id);
                    outcomes.insert(account_id, MemberRefreshOutcome::Refreshed);
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

    pub(super) fn set_usage(
        &self,
        account_id: &str,
        regular_remaining: Option<u64>,
        spark_remaining: Option<u64>,
        last_refreshed: Instant,
    ) {
        self.apply_usage(
            account_id,
            MemberUsage {
                regular_remaining,
                spark_remaining,
                last_refreshed,
                ordinary_usage_allowed: None,
                spark_usage_allowed: None,
                credits: None,
                rate_limit_reset_credit_count: None,
                spend_control_reached: None,
            },
        );
    }

    pub(super) fn apply_usage(&self, account_id: &str, usage: MemberUsage) {
        if let Ok(mut guard) = self.state.write() {
            let state = guard.entry(account_id.to_string()).or_default();
            if usage.included_available(AccountPoolUsageBucket::Regular) == Some(true) {
                state.regular_exhausted = false;
            }
            if usage.included_available(AccountPoolUsageBucket::Spark) == Some(true) {
                state.spark_exhausted = false;
            }
            state.usage = Some(usage);
        }
    }
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
    auth_route_config: AuthRouteConfig,
) -> Result<(String, MemberUsage), MemberRefreshError> {
    let auth = manager
        .auth_unpooled_cached_for_account_pool_member()
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
    let result = tokio::time::timeout(
        USAGE_REQUEST_TIMEOUT,
        fetch_usage_remaining(&base_url, &auth, &auth_route_config),
    )
    .await
    .unwrap_or_else(|_| {
        Err(
            super::transport::UsageRefreshFailure::with_usable_credentials(
                "usage refresh timed out".to_string(),
            ),
        )
    });
    match result {
        Ok(usage) => Ok((account_id, usage)),
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

pub(super) async fn fetch_usage_remaining(
    base_url: &str,
    auth: &CodexAuth,
    auth_route_config: &AuthRouteConfig,
) -> Result<MemberUsage, super::transport::UsageRefreshFailure> {
    let payload = super::transport::request(
        base_url,
        auth,
        auth_route_config,
        super::transport::UsageOperation::Read,
    )
    .await?;
    parse_usage(&payload)
}

pub(super) fn parse_usage(
    payload: &Value,
) -> Result<MemberUsage, super::transport::UsageRefreshFailure> {
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
    Ok(MemberUsage {
        regular_remaining,
        spark_remaining,
        last_refreshed: Instant::now(),
        ordinary_usage_allowed: usage_allowed(payload.get("rate_limit")),
        spark_usage_allowed: payload
            .get("additional_rate_limits")
            .and_then(Value::as_array)
            .and_then(|limits| {
                limits.iter().find_map(|limit| {
                    (limit.get("metered_feature").and_then(Value::as_str) != Some("codex"))
                        .then(|| usage_allowed(limit.get("rate_limit")))
                        .flatten()
                })
            }),
        credits: payload
            .get("credits")
            .filter(|credits| !credits.is_null())
            .map(|credits| serde_json::from_value(credits.clone()))
            .transpose()
            .map_err(|err| {
                super::transport::UsageRefreshFailure::with_usable_credentials(format!(
                    "invalid credits: {err}"
                ))
            })?,
        rate_limit_reset_credit_count: payload
            .pointer("/rate_limit_reset_credits/available_count")
            .and_then(Value::as_i64),
        spend_control_reached: payload
            .pointer("/spend_control/reached")
            .and_then(Value::as_bool),
    })
}

pub(super) fn remaining_from_rate_limit(rate_limit: Option<&Value>) -> Option<u64> {
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

impl MemberUsage {
    pub(super) fn included_available(&self, bucket: AccountPoolUsageBucket) -> Option<bool> {
        let (allowed, remaining) = match bucket {
            AccountPoolUsageBucket::Regular => {
                (self.ordinary_usage_allowed, self.regular_remaining)
            }
            AccountPoolUsageBucket::Spark => (self.spark_usage_allowed, self.spark_remaining),
        };
        allowed.or_else(|| remaining.map(|remaining| remaining > 0))
    }

    pub(super) fn credits_available(&self) -> bool {
        self.spend_control_reached != Some(true)
            && self.credits.as_ref().is_some_and(|credits| {
                credits.unlimited
                    || (credits.has_credits
                        && credits.balance.as_deref().is_none_or(|balance| {
                            decimal_parts(balance).map_or_else(
                                || balance.parse::<f64>().map_or(true, |value| value > 0.0),
                                |(integer, fraction)| !integer.is_empty() || !fraction.is_empty(),
                            )
                        }))
            })
    }
}

// Compare decimal strings without rounding the backend's exact credit balance.
pub(super) fn decimal_parts(value: &str) -> Option<(&str, &str)> {
    let value = value.trim().strip_prefix('+').unwrap_or(value.trim());
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((
        integer.trim_start_matches('0'),
        fraction.trim_end_matches('0'),
    ))
}

fn usage_allowed(rate_limit: Option<&Value>) -> Option<bool> {
    let rate_limit = rate_limit?;
    rate_limit
        .get("allowed")
        .and_then(Value::as_bool)
        .or_else(|| {
            rate_limit
                .get("limit_reached")
                .and_then(Value::as_bool)
                .map(|reached| !reached)
        })
        .or_else(|| {
            ["primary_window", "secondary_window"]
                .into_iter()
                .filter_map(|window| rate_limit.get(window)?.get("used_percent")?.as_f64())
                .map(|used| used < 100.0)
                .reduce(|left, right| left && right)
        })
}
