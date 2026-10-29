//! Live usage for account listings. Each credential home is loaded and queried once.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use codex_app_server_protocol::RateLimitSnapshot;
use codex_backend_client::Client as BackendClient;
use codex_core::config::Config;
use serde::Serialize;

use super::AccountCredential;
use super::CredentialStatus;
use super::credential_for_account_home;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AccountUsage {
    status: UsageStatus,
    error: Option<&'static str>,
    rate_limits: Option<Vec<RateLimitSnapshot>>,
    ordinary_usage_allowed: Option<bool>,
    rate_limit_reset_credit_count: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
enum UsageStatus {
    Available,
    #[default]
    Unavailable,
    Error,
}

pub(super) enum CredentialRequirement {
    Any,
    Chatgpt,
}

pub(super) async fn lookup(
    config: &Config,
    home: &Path,
    requirement: CredentialRequirement,
) -> AccountCredential {
    let mut credential = AccountCredential {
        status: CredentialStatus::Missing,
        auth_mode: None,
        usage: AccountUsage::default(),
    };
    // Include credential loading in the deadline, including remote authentication routes.
    let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        let (loaded, auth) = credential_for_account_home(
            home,
            config,
            matches!(requirement, CredentialRequirement::Chatgpt),
        )
        .await;
        credential = loaded;
        let Some(auth) = auth.filter(|_| credential.status == CredentialStatus::LoggedIn) else {
            return;
        };
        if !auth.uses_codex_backend() {
            return;
        }
        match BackendClient::from_auth(
            config.chatgpt_base_url.clone(),
            &auth,
            config.http_client_factory(),
        )
        .get_rate_limits_with_reset_credits()
        .await
        {
            Ok(usage) => {
                credential.usage = AccountUsage {
                    status: UsageStatus::Available,
                    error: None,
                    rate_limits: Some(
                        usage
                            .rate_limits
                            .into_iter()
                            .map(RateLimitSnapshot::from)
                            .collect(),
                    ),
                    ordinary_usage_allowed: usage.ordinary_usage_allowed,
                    rate_limit_reset_credit_count: usage
                        .rate_limit_reset_credits
                        .map(|credits| credits.available_count),
                };
            }
            Err(_) => {
                // Backend errors can contain response bodies. Keep account output concise.
                credential.usage.status = UsageStatus::Error;
                credential.usage.error = Some("usage request failed");
            }
        }
    })
    .await;
    if result.is_err() {
        if credential.status == CredentialStatus::Missing {
            credential.status = CredentialStatus::Invalid;
        }
        credential.usage.status = UsageStatus::Error;
        credential.usage.error = Some("usage lookup timed out after 10s");
    }
    credential
}

pub(super) fn render(usage: &AccountUsage) -> String {
    let mut output = String::new();
    match usage.status {
        UsageStatus::Unavailable => return "    usage: unavailable\n".to_string(),
        UsageStatus::Error => {
            return format!(
                "    usage: error: {}\n",
                usage.error.unwrap_or("unknown error")
            );
        }
        UsageStatus::Available => {}
    }
    for snapshot in usage.rate_limits.iter().flatten() {
        let name = snapshot
            .limit_name
            .as_deref()
            .or(snapshot.limit_id.as_deref())
            .unwrap_or("Codex");
        let _ = writeln!(output, "    {name}:");
        if let Some(model) = &snapshot.normal_model_slug {
            let _ = writeln!(output, "      model: {model}");
        }
        let plan = serde_json::json!(snapshot.plan_type);
        let _ = writeln!(output, "      plan: {}", plan.as_str().unwrap_or("unknown"));
        let credits = match &snapshot.credits {
            Some(credits) if credits.unlimited => "unlimited".to_string(),
            Some(credits) => format!(
                "{}; has credits: {}",
                credits.balance.as_deref().unwrap_or("hidden (unknown)"),
                credits.has_credits
            ),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(output, "      credits: {credits}");
        for (label, window) in [
            ("primary", &snapshot.primary),
            ("secondary", &snapshot.secondary),
        ] {
            if let Some(window) = window {
                let duration = window
                    .window_duration_mins
                    .map(|minutes| format!("{minutes}m"))
                    .unwrap_or_else(|| "unknown duration".to_string());
                let _ = writeln!(
                    output,
                    "      {label} ({duration}): {}% used; resets {}",
                    window.used_percent,
                    reset_time(window.resets_at)
                );
            } else {
                let _ = writeln!(output, "      {label}: unknown");
            }
        }
        if let Some(limit) = &snapshot.individual_limit {
            let _ = writeln!(
                output,
                "      spending limit: {}; used {}; {}% remaining; resets {}",
                limit.limit,
                limit.used,
                limit.remaining_percent,
                reset_time(Some(limit.resets_at))
            );
        }
        let _ = writeln!(
            output,
            "      spending limit reached: {}",
            known(snapshot.spend_control_reached)
        );
        if let Some(reason) = &snapshot.rate_limit_reached_type {
            let reason = serde_json::json!(reason);
            let _ = writeln!(
                output,
                "      reached-limit reason: {}",
                reason.as_str().unwrap_or("unknown")
            );
        }
    }
    let _ = writeln!(
        output,
        "    ordinary usage allowed: {}",
        known(usage.ordinary_usage_allowed)
    );
    let count = usage
        .rate_limit_reset_credit_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let _ = writeln!(output, "    limit-reset credits: {count}");
    output
}

fn known(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn reset_time(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, /*nsecs*/ 0))
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}
