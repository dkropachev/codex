use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_login::AccountPoolAuthSelection;
use codex_login::AccountPoolCacheHeat;
use codex_login::AccountPoolUsageBucket;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountPoolFailoverDecision {
    Switch(AccountPoolUsageBucket),
    Stay,
    NoSafeRetry,
}

pub(crate) fn decide_account_pool_failover(
    selection: Option<&AccountPoolAuthSelection>,
    error: &UsageLimitReachedError,
    visible_output_started: bool,
    now: DateTime<Utc>,
) -> AccountPoolFailoverDecision {
    if visible_output_started {
        return AccountPoolFailoverDecision::NoSafeRetry;
    }
    let Some(selection) = selection else {
        return AccountPoolFailoverDecision::NoSafeRetry;
    };
    let Some(snapshot) = error.rate_limits.as_deref() else {
        return AccountPoolFailoverDecision::NoSafeRetry;
    };
    let Some(wait) = longest_exhausted_window_wait(snapshot, error.resets_at.as_ref(), now) else {
        return AccountPoolFailoverDecision::NoSafeRetry;
    };
    let cache_heat = selection.cache_hint.unwrap_or_default().heat();
    if wait >= switch_threshold(cache_heat) {
        return AccountPoolFailoverDecision::Switch(bucket_for_snapshot(
            snapshot,
            selection.bucket,
        ));
    }
    AccountPoolFailoverDecision::Stay
}

fn longest_exhausted_window_wait(
    snapshot: &RateLimitSnapshot,
    error_resets_at: Option<&DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Duration> {
    exhausted_windows(snapshot)
        .filter_map(|window| estimated_wait(window, error_resets_at, now))
        .max()
}

fn exhausted_windows(snapshot: &RateLimitSnapshot) -> impl Iterator<Item = &RateLimitWindow> {
    [snapshot.primary.as_ref(), snapshot.secondary.as_ref()]
        .into_iter()
        .flatten()
        .filter(|window| window.used_percent >= 100.0)
}

fn estimated_wait(
    window: &RateLimitWindow,
    error_resets_at: Option<&DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Duration> {
    if let Some(resets_at) = window
        .resets_at
        .and_then(|resets_at| DateTime::<Utc>::from_timestamp(resets_at, 0))
    {
        return Some((resets_at - now).max(Duration::zero()));
    }
    if let Some(resets_at) = error_resets_at {
        return Some((*resets_at - now).max(Duration::zero()));
    }
    let window_minutes = window.window_minutes?;
    Some(Duration::minutes(window_minutes.clamp(1, 10_080)))
}

fn switch_threshold(cache_heat: AccountPoolCacheHeat) -> Duration {
    match cache_heat {
        AccountPoolCacheHeat::Cold => Duration::zero(),
        AccountPoolCacheHeat::Warm => Duration::minutes(2),
        AccountPoolCacheHeat::Hot => Duration::minutes(10),
    }
}

fn bucket_for_snapshot(
    snapshot: &RateLimitSnapshot,
    fallback: AccountPoolUsageBucket,
) -> AccountPoolUsageBucket {
    match snapshot.limit_id.as_deref() {
        Some("codex") => AccountPoolUsageBucket::Regular,
        Some(_) => AccountPoolUsageBucket::Spark,
        None => fallback,
    }
}

#[cfg(test)]
#[path = "account_pool_failover_tests.rs"]
mod tests;
