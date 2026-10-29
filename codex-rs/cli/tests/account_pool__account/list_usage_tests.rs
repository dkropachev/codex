use super::support;

use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use support::account::TestHttpResponse;
use support::account::codex_command;
use support::account::start_http_server;
use support::account::write_api_key_auth;
use support::account::write_chatgpt_auth;
use support::account::write_config;
use support::account::write_invalid_auth;
use tempfile::TempDir;

#[test]
fn account_list_fetches_concurrently_once_per_home_with_own_auth_in_stable_order() -> Result<()> {
    let home = TempDir::new()?;
    write_config(
        home.path(),
        r#"
[account_pool]
enabled = true
[account_pool.pools.a]
provider = "openai"
accounts = ["work"]
policy = "drain"
[account_pool.pools.z]
provider = "openai"
accounts = ["work"]
policy = "drain"
"#,
    )?;
    let mut tokens = std::collections::BTreeMap::new();
    for id in ["default", "work", "standalone"] {
        let path = if id == "default" {
            home.path().to_path_buf()
        } else {
            home.path().join("accounts").join(id)
        };
        write_chatgpt_auth(&path, id, &format!("{id}@example.com"))?;
        let auth: Value = serde_json::from_slice(&std::fs::read(path.join("auth.json"))?)?;
        let token = auth["tokens"]["access_token"]
            .as_str()
            .context("access token")?;
        tokens.insert(id.to_string(), format!("Bearer {token}"));
    }
    let gate = Arc::new((Mutex::new(/*t*/ 0), Condvar::new()));
    let server = start_http_server(/*expected_requests*/ 3, move |request| {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path, "/api/codex/usage");
        let (lock, wake) = &*gate;
        let mut arrived = lock
            .lock()
            .unwrap_or_else(|err| panic!("synchronization lock poisoned: {err}"));
        *arrived += 1;
        wake.notify_all();
        let (arrived, _) = wake
            .wait_timeout_while(arrived, Duration::from_secs(/*secs*/ 5), |count| *count < 3)
            .unwrap_or_else(|err| panic!("synchronization wait failed: {err}"));
        assert_eq!(
            *arrived, 3,
            "all accounts must arrive before any response is sent"
        );
        TestHttpResponse::json(
            /*status_code*/ 200,
            r#"{"plan_type":"pro","rate_limit":{"allowed":true,"limit_reached":false},"rate_limit_reset_credits":{"available_count":0}}"#,
        )
    })?;
    let output = list_output(&home, &server.url, &["--json"])?;
    let value: Value = serde_json::from_slice(&output)?;
    let ids = value["accounts"]
        .as_array()
        .context("accounts array")?
        .iter()
        .map(|account| account["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        json!(ids),
        json!(["default", "a", "z", "work", "standalone"])
    );
    assert_eq!(
        value["pools"][0]["members"][0]["usage"],
        value["pools"][1]["members"][0]["usage"]
    );
    assert_eq!(value["accounts"][3]["usage"]["ordinaryUsageAllowed"], true);
    assert_eq!(
        value["accounts"][3]["usage"]["rateLimitResetCreditCount"],
        0
    );
    let mut requested_auth = server
        .finish()?
        .iter()
        .map(|request| {
            (
                request.header("chatgpt-account-id"),
                request.header("authorization"),
            )
        })
        .collect::<Vec<_>>();
    requested_auth.sort();
    assert_eq!(
        requested_auth,
        tokens
            .into_iter()
            .map(|(id, token)| (Some(id), Some(token)))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn account_list_distinguishes_credit_states_and_unknown_limits() -> Result<()> {
    let home = TempDir::new()?;
    write_config(home.path(), "")?;
    let hidden = json!({"has_credits": true, "unlimited": false, "balance": null});
    let unlimited = json!({"has_credits": true, "unlimited": true, "balance": null});
    let zero = json!({"has_credits": false, "unlimited": false, "balance": "0.000"});
    let states = [
        ("hidden", hidden),
        ("missing", Value::Null),
        ("unlimited", unlimited),
        ("zero", zero),
    ];
    for (id, _) in &states {
        write_chatgpt_auth(
            &home.path().join("accounts").join(id),
            id,
            "test@example.com",
        )?;
    }
    let responses = states.clone();
    let server = start_http_server(/*expected_requests*/ 8, move |request| {
        let id = request
            .header("chatgpt-account-id")
            .unwrap_or_else(|| panic!("missing account header"));
        let credits = &responses
            .iter()
            .find(|(name, _)| *name == id)
            .unwrap_or_else(|| panic!("unexpected account {id}"))
            .1;
        TestHttpResponse::json(
            /*status_code*/ 200,
            &json!({"plan_type": "free", "credits": credits}).to_string(),
        )
    })?;
    let mut expected_credits = Vec::new();
    for (_, credits) in states {
        let credits = if credits.is_null() {
            Value::Null
        } else {
            json!({
                "hasCredits": credits["has_credits"], "unlimited": credits["unlimited"], "balance": credits["balance"]
            })
        };
        expected_credits.push(credits);
    }
    let output = list_output(&home, &server.url, &["--json"])?;
    let value: Value = serde_json::from_slice(&output)?;
    let credits = value["accounts"]
        .as_array()
        .context("accounts array")?
        .iter()
        .map(|account| account["usage"]["rateLimits"][0]["credits"].clone())
        .collect::<Vec<_>>();
    assert_eq!(credits, expected_credits);
    let output = list_output(&home, &server.url, &[])?;
    let output = String::from_utf8(output)?;
    let snapshot = codex_utils_cargo_bin::find_resource!(
        "tests/account_pool__account/snapshots/account_pool__account__list_usage__account_list_distinguishes_credit_states_and_unknown_limits.snap"
    )?;
    insta::with_settings!({snapshot_path => snapshot.parent().context("snapshot has no parent")?}, {
        insta::assert_snapshot!(output);
    });
    assert_eq!(server.finish()?.len(), 8);
    Ok(())
}

#[test]
fn account_list_retains_credentials_when_usage_fails_or_times_out() -> Result<()> {
    let home = TempDir::new()?;
    write_config(home.path(), "")?;
    for id in ["backend-error", "malformed", "timeout"] {
        write_chatgpt_auth(
            &home.path().join("accounts").join(id),
            id,
            "test@example.com",
        )?;
    }
    write_api_key_auth(&home.path().join("accounts/api-key"))?;
    write_invalid_auth(&home.path().join("accounts/invalid"))?;
    let pat_home = home.path().join("accounts/auth-timeout");
    std::fs::create_dir_all(&pat_home)?;
    std::fs::write(
        pat_home.join("auth.json"),
        r#"{"personal_access_token":"at-timeout-fixture"}"#,
    )?;
    let server = start_http_server(/*expected_requests*/ 4, |request| {
        if request.path == "/v1/user-auth-credential/whoami" {
            assert_eq!(
                request.header("authorization").as_deref(),
                Some("Bearer at-timeout-fixture")
            );
            std::thread::sleep(Duration::from_secs(/*secs*/ 11));
            return TestHttpResponse::json(/*status_code*/ 200, "{}");
        }
        match request.header("chatgpt-account-id").as_deref() {
            Some("backend-error") => {
                TestHttpResponse::text(/*status_code*/ 403, "private backend details")
            }
            Some("malformed") => TestHttpResponse::json(/*status_code*/ 200, "{"),
            Some("timeout") => {
                std::thread::sleep(Duration::from_secs(/*secs*/ 11));
                TestHttpResponse::json(/*status_code*/ 200, r#"{"plan_type":"pro"}"#)
            }
            id => panic!("unexpected account {id:?}"),
        }
    })?;
    let output = list_output(&home, &server.url, &["--json"])?;
    let value: Value = serde_json::from_slice(&output)?;
    let actual = value["accounts"]
        .as_array()
        .context("accounts array")?
        .iter()
        .map(|account| json!([account["id"], account["credentialStatus"], account["usage"]]))
        .collect::<Vec<_>>();
    let usage = |status, error| {
        json!({"status": status, "error": error, "rateLimits": null,
        "ordinaryUsageAllowed": null, "rateLimitResetCreditCount": null})
    };
    let unavailable = usage("unavailable", /*error*/ None);
    let error = usage("error", Some("usage request failed"));
    let timeout = usage("error", Some("usage lookup timed out after 10s"));
    assert_eq!(
        actual,
        vec![
            json!(["api-key", "logged in", unavailable]),
            json!(["auth-timeout", "invalid", timeout]),
            json!(["backend-error", "logged in", error]),
            json!(["invalid", "invalid", unavailable]),
            json!(["malformed", "logged in", error]),
            json!(["timeout", "logged in", timeout]),
        ]
    );
    assert_eq!(server.finish()?.len(), 4);
    Ok(())
}

fn list_output(home: &TempDir, url: &str, args: &[&str]) -> Result<Vec<u8>> {
    Ok(codex_command(home.path())?
        .env("CODEX_AUTHAPI_BASE_URL", url)
        .args([
            "-c",
            &format!("chatgpt_base_url={url:?}"),
            "account",
            "list",
        ])
        .args(args)
        .timeout(Duration::from_secs(/*secs*/ 20))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone())
}
