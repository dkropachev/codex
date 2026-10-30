use super::support;

use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use support::account::codex_command;
use support::account::write_chatgpt_auth;
use support::account::write_config;
use support::account::write_expired_chatgpt_auth;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_list_refreshes_selected_credentials_before_fetching_usage() -> Result<()> {
    enum DefaultCredentials {
        Stored,
        Environment,
    }
    for default_credentials in [DefaultCredentials::Stored, DefaultCredentials::Environment] {
        let home = TempDir::new()?;
        let server = MockServer::start().await;
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
        for id in ["default", "work", "standalone", "fresh"] {
            let account_home = if id == "default" {
                home.path().to_path_buf()
            } else {
                home.path().join("accounts").join(id)
            };
            if id == "fresh" {
                write_chatgpt_auth(&account_home, id, "test@example.com")?;
            } else {
                write_expired_chatgpt_auth(&account_home, id, "test@example.com")?;
            }
            let stored: Value =
                serde_json::from_slice(&std::fs::read(account_home.join("auth.json"))?)?;
            let (usage_id, token) = if id == "default"
                && matches!(default_credentials, DefaultCredentials::Environment)
            {
                Mock::given(method("GET"))
                    .and(path("/v1/user-auth-credential/whoami"))
                    .and(header("authorization", "Bearer at-env-fixture"))
                    .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                        "email": "environment@example.com",
                        "chatgpt_user_id": "environment-user",
                        "chatgpt_account_id": "environment",
                        "chatgpt_plan_type": "pro",
                        "chatgpt_account_is_fedramp": false,
                    })))
                    .expect(/*r*/ 1)
                    .mount(&server)
                    .await;
                ("environment", "at-env-fixture".to_string())
            } else if id == "fresh" {
                (
                    id,
                    stored["tokens"]["access_token"]
                        .as_str()
                        .context("token")?
                        .to_string(),
                )
            } else {
                let token = format!("refreshed-{id}");
                Mock::given(method("POST"))
                    .and(path("/oauth/token"))
                    .and(body_partial_json(
                        json!({"refresh_token": format!("refresh-{id}")}),
                    ))
                    .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                        "access_token": token,
                        "refresh_token": format!("rotated-{id}"),
                    })))
                    .expect(/*r*/ 1)
                    .mount(&server)
                    .await;
                (id, token)
            };
            Mock::given(method("GET"))
                .and(path("/api/codex/usage"))
                .and(header("chatgpt-account-id", usage_id))
                .and(header("authorization", format!("Bearer {token}")))
                .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                    "plan_type": "pro",
                    "credits": {"has_credits": true, "unlimited": false, "balance": "12.345"},
                })))
                .expect(/*r*/ 1)
                .mount(&server)
                .await;
        }
        let mut cmd = codex_command(home.path())?;
        cmd.env_remove("CODEX_ACCESS_TOKEN");
        if matches!(default_credentials, DefaultCredentials::Environment) {
            cmd.env("CODEX_ACCESS_TOKEN", "at-env-fixture");
        }
        let output = cmd
            .env("CODEX_AUTHAPI_BASE_URL", server.uri())
            .env(
                codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
                format!("{}/oauth/token", server.uri()),
            )
            .args([
                "-c",
                &format!("chatgpt_base_url={:?}", server.uri()),
                "account",
                "list",
                "--json",
            ])
            .timeout(Duration::from_secs(/*secs*/ 20))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let value: Value = serde_json::from_slice(&output)?;
        let summaries = value["accounts"]
            .as_array()
            .context("accounts")?
            .iter()
            .filter(|account| account["type"] == "account")
            .map(|account| {
                json!([
                    account["id"],
                    account["credentialStatus"],
                    account["usage"]["status"],
                    account["usage"]["rateLimits"][0]["credits"]["balance"]
                ])
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summaries,
            vec![
                json!(["default", "logged in", "available", "12.345"]),
                json!(["work", "logged in", "available", "12.345"]),
                json!(["fresh", "logged in", "available", "12.345"]),
                json!(["standalone", "logged in", "available", "12.345"]),
            ]
        );
        assert_eq!(value["pools"][0]["members"], value["pools"][1]["members"]);
        for id in ["work", "standalone"] {
            let stored: Value = serde_json::from_slice(&std::fs::read(
                home.path().join("accounts").join(id).join("auth.json"),
            )?)?;
            assert_eq!(stored["tokens"]["refresh_token"], format!("rotated-{id}"));
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_list_isolates_refresh_failures_and_timeouts() -> Result<()> {
    let home = TempDir::new()?;
    let server = MockServer::start().await;
    write_config(home.path(), "")?;
    for id in ["rejected", "timeout"] {
        write_expired_chatgpt_auth(
            &home.path().join("accounts").join(id),
            id,
            "test@example.com",
        )?;
        let response = if id == "timeout" {
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"access_token": "late-token"}))
                .set_delay(Duration::from_secs(/*secs*/ 11))
        } else {
            ResponseTemplate::new(/*s*/ 401)
                .set_body_json(json!({"error": {"code": "refresh_token_expired"}}))
        };
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_partial_json(
                json!({"refresh_token": format!("refresh-{id}")}),
            ))
            .respond_with(response)
            .expect(/*r*/ 1)
            .mount(&server)
            .await;
    }
    write_chatgpt_auth(
        &home.path().join("accounts/healthy"),
        "healthy",
        "test@example.com",
    )?;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("chatgpt-account-id", "healthy"))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"plan_type": "pro"})))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    let output = codex_command(home.path())?
        .env_remove("CODEX_ACCESS_TOKEN")
        .env(
            codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{}/oauth/token", server.uri()),
        )
        .args([
            "-c",
            &format!("chatgpt_base_url={:?}", server.uri()),
            "account",
            "list",
            "--json",
        ])
        .timeout(Duration::from_secs(/*secs*/ 20))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: Value = serde_json::from_slice(&output)?;
    let summaries = value["accounts"]
        .as_array()
        .context("accounts")?
        .iter()
        .map(|account| {
            json!([
                account["id"],
                account["credentialStatus"],
                account["usage"]["status"],
                account["usage"]["error"]
            ])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summaries,
        vec![
            json!(["healthy", "logged in", "available", null]),
            json!(["rejected", "logged in", "error", "usage request failed"]),
            json!([
                "timeout",
                "logged in",
                "error",
                "usage lookup timed out after 10s"
            ]),
        ]
    );
    assert_eq!(
        server.received_requests().await.context("requests")?.len(),
        3
    );
    Ok(())
}
