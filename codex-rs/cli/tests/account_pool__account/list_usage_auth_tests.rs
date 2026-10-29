use super::support;

use std::collections::BTreeMap;
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
fn account_list_keeps_environment_default_and_named_home_identity() -> Result<()> {
    enum DefaultCredentials {
        Stored,
        EnvironmentOnly,
    }
    for default_credentials in [
        DefaultCredentials::Stored,
        DefaultCredentials::EnvironmentOnly,
    ] {
        let home = TempDir::new()?;
        write_config(
            home.path(),
            r#"
[account_pool]
enabled = true
[account_pool.pools.a]
provider = "openai"
accounts = ["work", "missing"]
policy = "drain"
[account_pool.pools.z]
provider = "openai"
accounts = ["work"]
policy = "drain"
"#,
        )?;
        if matches!(default_credentials, DefaultCredentials::Stored) {
            write_chatgpt_auth(home.path(), "stored-default", "default@example.com")?;
        }
        let mut credentials = BTreeMap::from([
            (
                "environment".to_string(),
                "Bearer at-env-fixture".to_string(),
            ),
            ("pat".to_string(), "Bearer at-stored-fixture".to_string()),
        ]);
        for id in ["work", "standalone"] {
            let account_home = home.path().join("accounts").join(id);
            write_chatgpt_auth(&account_home, id, &format!("{id}@example.com"))?;
            let auth: Value =
                serde_json::from_slice(&std::fs::read(account_home.join("auth.json"))?)?;
            let token = auth["tokens"]["access_token"]
                .as_str()
                .context("access token")?;
            credentials.insert(id.to_string(), format!("Bearer {token}"));
        }
        write_api_key_auth(&home.path().join("accounts/api-key"))?;
        write_invalid_auth(&home.path().join("accounts/invalid"))?;
        let pat_home = home.path().join("accounts/pat");
        std::fs::create_dir_all(&pat_home)?;
        std::fs::write(
            pat_home.join("auth.json"),
            r#"{"personal_access_token":"at-stored-fixture"}"#,
        )?;

        for args in [&["--json"][..], &[][..]] {
            let expected_credentials = credentials.clone();
            let server =
                start_http_server(/*expected_requests*/ 6, move |request| {
                    assert_eq!(request.method, "GET");
                    if request.path == "/v1/user-auth-credential/whoami" {
                        let id = match request.header("authorization").as_deref() {
                            Some("Bearer at-env-fixture") => "environment",
                            Some("Bearer at-stored-fixture") => "pat",
                            other => panic!("unexpected metadata auth: {other:?}"),
                        };
                        return TestHttpResponse::json(
                            /*status_code*/ 200,
                            &json!({
                                "email": format!("{id}@example.com"),
                                "chatgpt_user_id": format!("user-{id}"),
                                "chatgpt_account_id": id,
                                "chatgpt_plan_type": "pro",
                                "chatgpt_account_is_fedramp": false,
                            })
                            .to_string(),
                        );
                    }
                    assert_eq!(request.path, "/api/codex/usage");
                    let id = request
                        .header("chatgpt-account-id")
                        .unwrap_or_else(|| panic!("missing account header"));
                    assert_eq!(
                        request.header("authorization"),
                        Some(expected_credentials[&id].clone())
                    );
                    let balance = match id.as_str() {
                        "environment" => "90.00",
                        "work" => "20.00",
                        "pat" => "30.00",
                        "standalone" => "40.00",
                        other => panic!("unexpected usage account: {other}"),
                    };
                    TestHttpResponse::json(/*status_code*/ 200, &json!({
                    "plan_type": "pro",
                    "credits": {"has_credits": true, "unlimited": false, "balance": balance},
                }).to_string())
                })?;
            let output = codex_command(home.path())?
                .env("CODEX_ACCESS_TOKEN", "at-env-fixture")
                .env("CODEX_AUTHAPI_BASE_URL", &server.url)
                .args([
                    "-c",
                    &format!("chatgpt_base_url={:?}", server.url),
                    "account",
                    "list",
                ])
                .args(args)
                .timeout(Duration::from_secs(/*secs*/ 20))
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            if args.is_empty() {
                let output = String::from_utf8(output)?;
                let snapshot = codex_utils_cargo_bin::find_resource!(
                    "tests/account_pool__account/snapshots/account_pool__account__list_usage_auth__account_list_keeps_environment_default_and_named_home_identity.snap"
                )?;
                insta::with_settings!({snapshot_path => snapshot.parent().context("snapshot has no parent")?}, {
                    insta::assert_snapshot!(
                        "account_list_keeps_environment_default_and_named_home_identity",
                        output
                    );
                });
            } else {
                let value: Value = serde_json::from_slice(&output)?;
                let accounts = value["accounts"].as_array().context("accounts")?;
                let summaries = accounts
                    .iter()
                    .filter(|account| account["type"] == "account")
                    .map(|account| {
                        json!([
                            account["id"],
                            account["credentialStatus"],
                            account["authMode"],
                            account["usage"]["rateLimits"][0]["credits"]["balance"],
                        ])
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    json!(summaries),
                    json!([
                        ["default", "logged in", "personalAccessToken", "90.00"],
                        ["work", "logged in", "chatgpt", "20.00"],
                        ["missing", "missing", null, null],
                        ["api-key", "logged in", "apiKey", null],
                        ["invalid", "invalid", null, null],
                        ["pat", "logged in", "personalAccessToken", "30.00"],
                        ["standalone", "logged in", "chatgpt", "40.00"],
                    ])
                );
                let work = accounts
                    .iter()
                    .find(|account| account["id"] == "work")
                    .context("work")?;
                for pool in value["pools"].as_array().context("pools")? {
                    assert_eq!(
                        pool["members"][0],
                        json!({
                            "id": "work", "credentialStatus": "logged in", "authMode": "chatgpt",
                            "usage": work["usage"],
                        })
                    );
                }
            }
            let requests = server.finish()?;
            let mut actual = requests
                .iter()
                .map(|request| {
                    (
                        request.path.clone(),
                        request.header("chatgpt-account-id"),
                        request.header("authorization"),
                    )
                })
                .collect::<Vec<_>>();
            actual.sort();
            let mut expected = credentials
                .iter()
                .map(|(id, bearer)| {
                    (
                        "/api/codex/usage".to_string(),
                        Some(id.clone()),
                        Some(bearer.clone()),
                    )
                })
                .collect::<Vec<_>>();
            for bearer in ["at-env-fixture", "at-stored-fixture"] {
                expected.push((
                    "/v1/user-auth-credential/whoami".to_string(),
                    None,
                    Some(format!("Bearer {bearer}")),
                ));
            }
            expected.sort();
            assert_eq!(actual, expected);
        }
    }
    Ok(())
}
