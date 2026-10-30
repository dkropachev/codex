mod support;

#[path = "account_pool__account/list_usage_tests.rs"]
mod list_usage;

#[path = "account_pool__account/list_usage_auth_tests.rs"]
mod list_usage_auth;

#[path = "account_pool__account/list_usage_refresh_tests.rs"]
mod list_usage_refresh;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
use support::account::TestHttpResponse;
use support::account::account_pool_config;
use support::account::codex_command;
use support::account::start_http_server;
use support::account::write_api_key_auth;
use support::account::write_chatgpt_auth;
use support::account::write_config;
use support::account::write_expired_chatgpt_auth;
use support::account::write_invalid_auth;
use tempfile::TempDir;

#[test]
fn login_status_uses_configured_account_pool_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["login", "status"])
        .assert()
        .success()
        .stdout("")
        .stderr("Logged in using ChatGPT\n");

    Ok(())
}

#[test]
fn account_list_human_groups_pool_members_and_statuses() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;
    write_chatgpt_auth(codex_home.path(), "default", "default@example.com")?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("standalone"),
        "standalone",
        "standalone@example.com",
    )?;

    let server = start_http_server(/*expected_requests*/ 3, |_| {
        TestHttpResponse::json(/*status_code*/ 200, &usage_payload().to_string())
    })?;
    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["-c", &format!("chatgpt_base_url={:?}", server.url)]);
    let output = cmd
        .args(["account", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    let reset_time =
        chrono::DateTime::from_timestamp(/*secs*/ 2_000_000_000, /*nsecs*/ 0)
            .context("valid reset timestamp")?
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string();
    let snapshot = codex_utils_cargo_bin::find_resource!(
        "tests/snapshots/account_pool__account__account_list_human_groups_pool_members_and_statuses.snap"
    )?;
    insta::with_settings!({snapshot_path => snapshot.parent().context("snapshot has no parent")?}, {
        insta::assert_snapshot!(stdout.replace(&reset_time, "[RESET]"));
    });

    assert_eq!(server.finish()?.len(), 3);
    Ok(())
}

#[test]
fn account_list_human_marks_invalid_pool_members() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(
        codex_home.path(),
        r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["api-key-pro", "corrupt-pro", "missing-pro"]
policy = "drain"
"#,
    )?;
    write_api_key_auth(&codex_home.path().join("accounts").join("api-key-pro"))?;
    write_invalid_auth(&codex_home.path().join("accounts").join("corrupt-pro"))?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout,
        "Pool codex-pro (default pool, provider openai, policy drain):\n  api-key-pro: invalid\n    usage: unavailable\n  corrupt-pro: invalid\n    usage: unavailable\n  missing-pro: missing\n    usage: unavailable\n"
    );

    Ok(())
}

#[test]
fn account_list_json_includes_pool_metadata_and_memberships() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;
    write_chatgpt_auth(codex_home.path(), "default", "default@example.com")?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("standalone"),
        "standalone",
        "standalone@example.com",
    )?;

    let server = start_http_server(/*expected_requests*/ 3, |_| {
        TestHttpResponse::json(/*status_code*/ 200, &usage_payload().to_string())
    })?;
    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["-c", &format!("chatgpt_base_url={:?}", server.url)]);
    let output = cmd
        .args(["account", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output)?;

    let unavailable = json!({"status": "unavailable", "error": null, "rateLimits": null,
        "ordinaryUsageAllowed": null, "rateLimitResetCreditCount": null});
    let usage = json!({
        "status": "available", "error": null, "ordinaryUsageAllowed": false,
        "rateLimitResetCreditCount": 3,
        "rateLimits": [{
            "limitId": "codex", "limitName": null, "normalModelSlug": null, "planType": "pro",
            "primary": {"usedPercent": 25, "windowDurationMins": 300, "resetsAt": 2000000000},
            "secondary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 2000000000},
            "credits": {"hasCredits": true, "unlimited": false, "balance": "12.345600"},
            "individualLimit": {"limit": "100.00", "used": "75.125", "remainingPercent": 25, "resetsAt": 2000000000},
            "spendControlReached": true, "rateLimitReachedType": "workspace_member_usage_limit_reached"
        }, {
            "limitId": "codex_spark", "limitName": "Spark", "normalModelSlug": "gpt-spark", "planType": "pro",
            "primary": {"usedPercent": 10, "windowDurationMins": 60, "resetsAt": 2000000000},
            "secondary": null, "credits": null, "individualLimit": null,
            "spendControlReached": null, "rateLimitReachedType": null
        }]
    });
    assert_eq!(
        value,
        json!({
            "accounts": [
                {
                    "id": "default",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "usage": usage,
                    "pools": [],
                    "poolMembership": []
                },
                {
                    "id": "codex-pro",
                    "type": "pool",
                    "default": true,
                    "provider": "openai",
                    "policy": "drain",
                    "members": ["work-pro", "personal-pro"]
                },
                {
                    "id": "work-pro",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "usage": usage,
                    "pools": ["codex-pro"],
                    "poolMembership": [
                        {
                            "poolId": "codex-pro",
                            "default": true,
                            "memberIndex": 0
                        }
                    ]
                },
                {
                    "id": "personal-pro",
                    "type": "account",
                    "credentialStatus": "missing",
                    "authMode": null,
                    "usage": unavailable,
                    "pools": ["codex-pro"],
                    "poolMembership": [
                        {
                            "poolId": "codex-pro",
                            "default": true,
                            "memberIndex": 1
                        }
                    ]
                },
                {
                    "id": "standalone",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
                    "usage": usage,
                    "pools": [],
                    "poolMembership": []
                }
            ],
            "pools": [
                {
                    "id": "codex-pro",
                    "default": true,
                    "provider": "openai",
                    "policy": "drain",
                    "memberIds": ["work-pro", "personal-pro"],
                    "members": [
                        {
                            "id": "work-pro",
                            "credentialStatus": "logged in",
                            "authMode": "chatgpt",
                            "usage": usage
                        },
                        {
                            "id": "personal-pro",
                            "credentialStatus": "missing",
                            "authMode": null,
                            "usage": unavailable
                        }
                    ]
                }
            ]
        })
    );

    assert_eq!(server.finish()?.len(), 3);
    Ok(())
}

#[test]
fn account_limits_groups_pool_members_and_reports_missing_invalid_in_config_order() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(
        codex_home.path(),
        r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["api-key-pro", "corrupt-pro", "missing-pro"]
policy = "drain"
"#,
    )?;
    write_api_key_auth(&codex_home.path().join("accounts").join("api-key-pro"))?;
    write_invalid_auth(&codex_home.path().join("accounts").join("corrupt-pro"))?;

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "limits"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    assert_eq!(
        stdout,
        "codex-pro (default pool, drain): 3 members, 1 missing credential, 2 invalid credentials\n\napi-key-pro (pool: codex-pro)\n  credentials: invalid\n  limits: unavailable\n\ncorrupt-pro (pool: codex-pro)\n  credentials: invalid\n  limits: unavailable\n\nmissing-pro (pool: codex-pro)\n  credentials: missing\n  limits: unavailable\n"
    );

    Ok(())
}

#[test]
fn account_refresh_pool_reports_all_missing_credentials() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/2 accounts in pool codex-pro; work-pro missing credentials; personal-pro missing credentials",
        ));

    Ok(())
}

#[test]
fn account_refresh_pool_reports_partial_success() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |_request| {
        TestHttpResponse::json(
            /*status_code*/ 200,
            r#"{"rate_limit":{"primary_window":{"used_percent":25.0}}}"#,
        )
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            account_pool_config("")
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .success()
        .stderr(contains(
            "Refreshed 1/2 accounts in pool codex-pro; personal-pro missing credentials",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 1);

    Ok(())
}

#[test]
fn account_refresh_pool_reports_blocked_member_and_succeeds_when_another_member_refreshes()
-> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 2, |request| {
        if request.header("ChatGPT-Account-ID").as_deref() == Some("personal-pro") {
            TestHttpResponse::text(/*status_code*/ 403, "account blocked")
        } else {
            TestHttpResponse::json(
                /*status_code*/ 200,
                r#"{"rate_limit":{"primary_window":{"used_percent":25.0}}}"#,
            )
        }
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            account_pool_config("")
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("personal-pro"),
        "personal-pro",
        "personal@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .success()
        .stderr(contains(
            "Refreshed 1/2 accounts in pool codex-pro; personal-pro failed to fetch codex usage: 403 Forbidden; body=account blocked",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 2);

    Ok(())
}

#[test]
fn account_refresh_pool_fails_when_all_members_are_blocked() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |_request| {
        TestHttpResponse::text(/*status_code*/ 403, "account blocked")
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro"]
policy = "drain"
"#
        ),
    )?;
    write_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/1 accounts in pool codex-pro; work-pro failed to fetch codex usage: 403 Forbidden; body=account blocked",
        ));
    let requests = server.finish()?;
    assert_eq!(requests.len(), 1);

    Ok(())
}

#[test]
fn account_refresh_pool_fails_when_stale_credentials_cannot_refresh() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 2, |request| {
        if request.path == "/oauth/token" {
            TestHttpResponse::json(
                /*status_code*/ 401,
                r#"{"error":{"code":"refresh_token_expired"}}"#,
            )
        } else {
            TestHttpResponse::text(/*status_code*/ 401, "token expired")
        }
    })?;
    write_config(
        codex_home.path(),
        &format!(
            "chatgpt_base_url = \"{}\"\n{}",
            server.url,
            r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro"]
policy = "drain"
"#
        ),
    )?;
    write_expired_chatgpt_auth(
        &codex_home.path().join("accounts").join("work-pro"),
        "work-pro",
        "work@example.com",
    )?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{}/oauth/token", server.url),
    );
    cmd.args(["account", "refresh", "--pool", "codex-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Refreshed 0/1 accounts in pool codex-pro; work-pro failed to fetch codex usage: 401 Unauthorized; body=token expired",
        ));
    let requests = server.finish()?;
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![("POST", "/oauth/token"), ("GET", "/api/codex/usage")]
    );

    Ok(())
}

#[test]
fn account_refresh_pool_reports_missing_pool() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), &account_pool_config(""))?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["account", "refresh", "--pool", "missing"])
        .assert()
        .failure()
        .stderr(contains("Account pool missing not found"));

    Ok(())
}

fn usage_payload() -> serde_json::Value {
    let window = |seconds, used| {
        json!({
            "used_percent": used, "limit_window_seconds": seconds,
            "reset_after_seconds": 60, "reset_at": 2000000000
        })
    };
    json!({
        "plan_type": "pro",
        "credits": {"has_credits": true, "unlimited": false, "balance": "12.345600"},
        "rate_limit": {"allowed": false, "limit_reached": true,
            "primary_window": window(/*seconds*/ 18000, /*used*/ 25), "secondary_window": window(/*seconds*/ 604800, /*used*/ 100)},
        "additional_rate_limits": [{"metered_feature": "codex_spark", "limit_name": "Spark",
            "normal_model_slug": "gpt-spark",
            "rate_limit": {"allowed": true, "limit_reached": false,
                "primary_window": window(/*seconds*/ 3600, /*used*/ 10)}}],
        "spend_control": {"reached": true, "individual_limit": {
            "limit": "100.00", "used": "75.125", "remaining": "24.875", "used_percent": 75, "remaining_percent": 25,
            "reset_after_seconds": 60, "reset_at": 2000000000}},
        "rate_limit_reached_type": {"type": "workspace_member_usage_limit_reached"},
        "rate_limit_reset_credits": {"available_count": 3}
    })
}
