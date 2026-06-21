#[allow(dead_code)]
mod support;

use anyhow::Result;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use support::account::TestHttpResponse;
use support::account::codex_command;
use support::account::start_http_server;
use support::account::write_config;
use support::account::write_expired_chatgpt_auth;
use tempfile::TempDir;

#[test]
fn account_refresh_named_account_fails_when_stale_credentials_cannot_refresh() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = start_http_server(/*expected_requests*/ 1, |request| {
        if request.path == "/oauth/token" {
            TestHttpResponse::json(
                /*status_code*/ 401,
                r#"{"error":{"code":"refresh_token_expired"}}"#,
            )
        } else {
            TestHttpResponse::text(/*status_code*/ 500, "unexpected request")
        }
    })?;
    write_config(codex_home.path(), "")?;
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
    cmd.args(["account", "refresh", "work-pro"])
        .assert()
        .failure()
        .stderr(contains(
            "Account work-pro invalid credentials: Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.",
        ));
    let requests = server.finish()?;
    assert_eq!(
        requests
            .iter()
            .map(|request| (request.method.as_str(), request.path.as_str()))
            .collect::<Vec<_>>(),
        vec![("POST", "/oauth/token")]
    );

    Ok(())
}

#[test]
fn account_help_mentions_new_flags_and_arguments() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_config(codex_home.path(), "")?;

    let mut login_help = codex_command(codex_home.path())?;
    login_help
        .args(["login", "--help"])
        .assert()
        .success()
        .stdout(contains("--device-auth"));

    let mut list_help = codex_command(codex_home.path())?;
    list_help
        .args(["account", "list", "--help"])
        .assert()
        .success()
        .stdout(contains("--json"));

    let mut refresh_help = codex_command(codex_home.path())?;
    refresh_help
        .args(["account", "refresh", "--help"])
        .assert()
        .success()
        .stdout(contains("[ID]").and(contains("--pool <POOL_ID>")));

    Ok(())
}
