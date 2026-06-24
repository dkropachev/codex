use chrono::Duration;
use codex_login::AccountPoolAuthSelection;
use codex_login::AccountPoolCacheHint;
use codex_login::AccountPoolUsageBucket;
use codex_login::CodexAuth;
use codex_protocol::error::UsageLimitReachedError;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn exhausted_short_window_can_switch_when_wait_exceeds_cache_cost() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(15),
        Some((now + Duration::minutes(15)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(/*cache_hint*/ None)),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Switch(AccountPoolUsageBucket::Regular)
    );
}

#[test]
fn exhausted_weekly_window_still_switches() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(7 * 24 * 60),
        Some((now + Duration::days(7)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(/*cache_hint*/ None)),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Switch(AccountPoolUsageBucket::Regular)
    );
}

#[test]
fn exhausted_hour_window_still_switches() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(60),
        Some((now + Duration::minutes(60)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(/*cache_hint*/ None)),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Switch(AccountPoolUsageBucket::Regular)
    );
}

#[test]
fn missing_window_reset_falls_back_to_window_duration() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(60),
        /*resets_at*/ None,
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }))),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Switch(AccountPoolUsageBucket::Regular)
    );
}

#[test]
fn missing_window_reset_uses_error_reset_before_window_duration() {
    let now = Utc::now();
    let mut error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(60),
        /*resets_at*/ None,
    ));
    error.resets_at = Some(now + Duration::minutes(5));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }))),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Stay
    );
}

#[test]
fn multiple_exhausted_windows_use_longest_wait() {
    let now = Utc::now();
    let mut error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(15),
        Some((now + Duration::minutes(1)).timestamp()),
    ));
    if let Some(rate_limits) = error.rate_limits.as_mut() {
        rate_limits.secondary = Some(rate_limit_window(
            /*used_percent*/ 100.0,
            Some(60),
            Some((now + Duration::minutes(20)).timestamp()),
        ));
    }

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }))),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Switch(AccountPoolUsageBucket::Regular)
    );
}

#[test]
fn hot_cache_prevents_short_wait_failover() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(15),
        Some((now + Duration::minutes(5)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }))),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::Stay
    );
}

#[test]
fn visible_output_prevents_failover() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 100.0,
        Some(60),
        Some((now + Duration::minutes(60)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(/*cache_hint*/ None)),
            &error,
            /*visible_output_started*/ true,
            now,
        ),
        AccountPoolFailoverDecision::NoSafeRetry
    );
}

#[test]
fn non_exhausted_window_does_not_switch() {
    let now = Utc::now();
    let error = usage_limit_error(rate_limit_window(
        /*used_percent*/ 99.9,
        Some(60),
        Some((now + Duration::minutes(60)).timestamp()),
    ));

    assert_eq!(
        decide_account_pool_failover(
            Some(&selection(/*cache_hint*/ None)),
            &error,
            /*visible_output_started*/ false,
            now,
        ),
        AccountPoolFailoverDecision::NoSafeRetry
    );
}

fn selection(cache_hint: Option<AccountPoolCacheHint>) -> AccountPoolAuthSelection {
    AccountPoolAuthSelection {
        auth: CodexAuth::from_api_key("sk-test"),
        pool_id: "codex-pro".to_string(),
        account_id: "work-pro".to_string(),
        bucket: AccountPoolUsageBucket::Regular,
        affinity_key: "thread-1".to_string(),
        cache_hint,
    }
}

fn usage_limit_error(primary: RateLimitWindow) -> UsageLimitReachedError {
    UsageLimitReachedError {
        plan_type: None,
        resets_at: None,
        rate_limits: Some(Box::new(RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            primary: Some(primary),
            secondary: None,
            credits: None,
            individual_limit: None,
            plan_type: None,
            rate_limit_reached_type: None,
        })),
        promo_message: None,
        rate_limit_reached_type: None,
    }
}

fn rate_limit_window(
    used_percent: f64,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
) -> RateLimitWindow {
    RateLimitWindow {
        used_percent,
        window_minutes,
        resets_at,
    }
}
