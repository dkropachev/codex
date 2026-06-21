mod support;

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
        "Default account:\n  default: logged in\n\nPool codex-pro (default pool, provider openai, policy drain):\n  work-pro: logged in\n  personal-pro: missing\n\nStandalone accounts:\n  standalone: logged in\n"
    );

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
        "Pool codex-pro (default pool, provider openai, policy drain):\n  api-key-pro: invalid\n  corrupt-pro: invalid\n  missing-pro: missing\n"
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

    let mut cmd = codex_command(codex_home.path())?;
    let output = cmd
        .args(["account", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output)?;

    assert_eq!(
        value,
        json!({
            "accounts": [
                {
                    "id": "default",
                    "type": "account",
                    "credentialStatus": "logged in",
                    "authMode": "chatgpt",
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
                            "authMode": "chatgpt"
                        },
                        {
                            "id": "personal-pro",
                            "credentialStatus": "missing",
                            "authMode": null
                        }
                    ]
                }
            ]
        })
    );

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
