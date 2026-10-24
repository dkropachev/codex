use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::CLIENT_ID;
use codex_login::CODEX_ACCESS_TOKEN_ENV_VAR;
use codex_login::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::login_with_bedrock_access_keys;
use codex_protocol::shell_environment::OPENAI_FEDERATION_RULE_ID_ENV_VAR;
use codex_protocol::shell_environment::OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

fn write_file_auth_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        "cli_auth_credentials_store = \"file\"\n",
    )?;
    Ok(())
}

fn read_auth_json(codex_home: &Path) -> Result<Value> {
    let auth_json = std::fs::read_to_string(codex_home.join("auth.json"))?;
    Ok(serde_json::from_str(&auth_json)?)
}

fn assert_keyring_logout_attempted_enterprise_cleanup(
    codex_home: &Path,
    output: &std::process::Output,
) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to remove enterprise authorization"),
        "{stderr}"
    );
    if output.status.success() {
        assert!(stderr.contains("Successfully logged out"), "{stderr}");
        assert!(!codex_home.join("auth.json").exists());
    } else {
        assert!(
            stderr.contains("Error logging out: failed to delete auth from keyring:"),
            "{stderr}"
        );
        assert!(codex_home.join("auth.json").exists());
    }
}

#[test]
fn login_with_api_key_reads_stdin_and_writes_auth_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    let login = cmd
        .args([
            "-c",
            "forced_login_method=\"api\"",
            "login",
            "--with-api-key",
        ])
        .write_stdin("sk-test\n")
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&login.get_output().stderr), @r"
    Reading API key from stdin...
    API key saved. Authentication is checked when you send a request.
    ");

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["OPENAI_API_KEY"], "sk-test");
    assert!(auth.get("tokens").is_none());
    assert!(auth.get("agent_identity").is_none());

    let status = codex_command(codex_home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(OPENAI_FEDERATION_RULE_ID_ENV_VAR)
        .env_remove(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR)
        .args(["login", "status"])
        .assert()
        .success();
    insta::assert_snapshot!(String::from_utf8_lossy(&status.get_output().stderr), @r"
    API key configured - ***
    Authentication is checked when you send a request.
    ");

    Ok(())
}

#[test]
fn api_key_auth_commands_work_when_log_dir_is_unavailable() -> Result<()> {
    let codex_home = TempDir::new()?;
    let log_dir = codex_home.path().join("unavailable-logs");
    std::fs::write(&log_dir, "not a directory")?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!("cli_auth_credentials_store = \"file\"\nlog_dir = {log_dir:?}\n"),
    )?;

    codex_command(codex_home.path())?
        .args(["login", "--with-api-key"])
        .write_stdin("sk-test\n")
        .assert()
        .success()
        .stderr(contains("Warning: failed to create login log directory"));
    assert_eq!(
        read_auth_json(codex_home.path())?["OPENAI_API_KEY"],
        "sk-test"
    );

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .success()
        .stderr(contains("API key"));

    codex_command(codex_home.path())?
        .arg("logout")
        .assert()
        .success()
        .stderr(contains("Successfully logged out"));
    assert!(!codex_home.path().join("auth.json").exists());

    Ok(())
}

#[test]
fn login_status_reports_auth_storage_errors() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(codex_home.path().join("auth.json"), "{invalid json")?;

    codex_command(codex_home.path())?
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("Error checking login status:"));

    Ok(())
}

#[test]
fn file_logout_does_not_inspect_retained_keyring_store() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    // Secrets storage cannot load through this path. File logout leaves it intact.
    std::fs::write(codex_home.path().join("secrets"), "not a directory")?;

    let output = codex_command(codex_home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .output()?;
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "Not logged in\n");

    Ok(())
}

#[test]
fn login_status_validates_configured_workload_identity() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    let missing_assertion = codex_home.path().join("missing-identity-token");

    codex_command(codex_home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env(OPENAI_FEDERATION_RULE_ID_ENV_VAR, "rule-test")
        .env(OPENAI_IDENTITY_TOKEN_FILE_ENV_VAR, &missing_assertion)
        .args(["login", "status"])
        .assert()
        .failure()
        .stderr(contains("could not read workload identity assertion file"));

    Ok(())
}

#[test]
fn logout_clears_only_the_selected_bedrock_provider() -> Result<()> {
    for (model_provider_id, managed_bedrock_auth, model) in [
        ("amazon-bedrock", true, "openai.gpt-5.6-sol"),
        ("amazon-bedrock-runtime", true, "global.openai.gpt-5.6-sol"),
        ("openai", true, "gpt-5.6-sol"),
        ("amazon-bedrock", false, "gpt-5.6-sol"),
        ("amazon-bedrock-runtime", false, "us.openai.gpt-5.6-sol"),
        ("openai", false, "gpt-5.6-sol"),
    ] {
        let codex_home = TempDir::new()?;
        let config_path = codex_home.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "cli_auth_credentials_store = \"file\"\n\
                 model_provider = \"{model_provider_id}\"\n\
                 model = \"{model}\"\n\
                 model_reasoning_effort = \"high\"\n\
                 [model_providers.amazon-bedrock]\n\
                 base_url = \"https://mantle.example.com/v1\"\n\
                 [model_providers.amazon-bedrock.aws]\n\
                 profile = \"mantle-profile\"\n\
                 region = \"us-west-2\"\n\
                 auth_refresh = {{ command = \"aws\", args = [\"sso\", \"login\"] }}\n\
                 [model_providers.amazon-bedrock-runtime]\n\
                 base_url = \"https://runtime.example.com/v1\"\n\
                 [model_providers.amazon-bedrock-runtime.aws]\n\
                 profile = \"runtime-profile\"\n\
                 region = \"us-east-1\"\n\
                 auth_refresh = {{ command = \"aws\", args = [\"login\"] }}\n"
            ),
        )?;
        if managed_bedrock_auth {
            login_with_bedrock_access_keys(
                codex_home.path(),
                "managed-access-key-id",
                "managed-secret-access-key",
                Some("managed-session-token"),
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
        }
        let mut expected_config: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        if model_provider_id != "openai" {
            let expected_root = expected_config
                .as_table_mut()
                .expect("config should be a table");
            expected_root.remove("model_provider");
            expected_root.remove("model");
            expected_root["model_providers"][model_provider_id]
                .as_table_mut()
                .expect("selected Bedrock provider should be a table")
                .remove("aws");
        }
        let expected_message = if managed_bedrock_auth || model_provider_id != "openai" {
            "Successfully logged out"
        } else {
            "Not logged in"
        };

        codex_command(codex_home.path())?
            .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
            .env("AWS_ACCESS_KEY_ID", "environment-access-key-id")
            .env("AWS_SECRET_ACCESS_KEY", "environment-secret-access-key")
            .args(["logout"])
            .assert()
            .success()
            .stderr(contains(expected_message));

        assert!(!codex_home.path().join("auth.json").exists());
        let actual_config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
        assert_eq!(actual_config, expected_config);
    }

    Ok(())
}

#[tokio::test]
async fn logout_survives_enterprise_cleanup_failure_with_xaa_disabled() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[features]\nuse_xaa = false\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n[mcp_servers.enterprise]\nurl = \"https://resource.example/mcp\"\nauth = \"ema_auth\"\nbearer_token_env_var = \"UNUSED_TOKEN\"\n",
            server.uri()
        ),
    )?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("account-access")
            .account_id("workspace")
            .chatgpt_user_id("user"),
        AuthCredentialsStoreMode::File,
    )?;
    // Fail before touching the real keyring; the subprocess owns this isolated home.
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;
    for enabled in [false, true] {
        codex_command(home.path())?
            .current_dir(home.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
            .args([
                "-c",
                &format!("features.use_xaa={enabled}"),
                "mcp",
                "logout",
                "enterprise",
            ])
            .assert()
            .failure()
            .stderr(contains("failed to delete enterprise authorization"));
        assert!(home.path().join("auth.json").exists());
    }
    codex_command(home.path())?
        .current_dir(home.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env(
            REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{}/oauth/revoke", server.uri()),
        )
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .assert()
        .success()
        .stderr(contains("continuing account logout"))
        .stderr(contains("Successfully logged out"));
    assert!(!home.path().join("auth.json").exists());
    Ok(())
}

#[test]
fn logout_named_account_preserves_default_bedrock_auth_and_config() -> Result<()> {
    let codex_home = TempDir::new()?;
    let named_account_home = codex_home.path().join("accounts").join("work");
    std::fs::create_dir_all(&named_account_home)?;
    let config_path = codex_home.path().join("config.toml");
    std::fs::write(
        &config_path,
        "cli_auth_credentials_store = \"file\"\n\
         model_provider = \"amazon-bedrock\"\n\
         model = \"openai.gpt-5.6-sol\"\n\
         [model_providers.amazon-bedrock]\n\
         base_url = \"https://bedrock.example.com/v1\"\n\
         [model_providers.amazon-bedrock.aws]\n\
         profile = \"default-profile\"\n\
         region = \"us-west-2\"\n",
    )?;
    login_with_bedrock_access_keys(
        codex_home.path(),
        "default-access-key-id",
        "default-secret-access-key",
        Some("default-session-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    login_with_bedrock_access_keys(
        &named_account_home,
        "named-access-key-id",
        "named-secret-access-key",
        Some("named-session-token"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let expected_default_auth = read_auth_json(codex_home.path())?;
    let expected_config: toml::Value = toml::from_str(&std::fs::read_to_string(&config_path)?)?;

    codex_command(codex_home.path())?
        .args(["logout", "--account", "work"])
        .assert()
        .success()
        .stderr(contains("Successfully logged out"));

    assert_eq!(read_auth_json(codex_home.path())?, expected_default_auth);
    assert!(!named_account_home.join("auth.json").exists());
    let actual_config: toml::Value = toml::from_str(&std::fs::read_to_string(config_path)?)?;
    assert_eq!(actual_config, expected_config);

    Ok(())
}

#[tokio::test]
async fn logout_cleans_up_enterprise_authorization_for_selected_accounts() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    let named_home = home.path().join("accounts/work");
    std::fs::create_dir_all(&named_home)?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n",
            server.uri()
        ),
    )?;
    // A failed lock acquisition makes each attempted grant cleanup observable
    // without reading or changing the host's keyring.
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;
    codex_login::login_with_api_key(
        home.path(),
        "sk-default",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    write_chatgpt_auth(
        &named_home,
        ChatGptAuthFixture::new("named-access")
            .account_id("named-workspace")
            .chatgpt_user_id("named-user"),
        AuthCredentialsStoreMode::File,
    )?;

    let output = codex_command(home.path())?
        .current_dir(home.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env(
            REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{}/oauth/revoke", server.uri()),
        )
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove("OPENAI_API_KEY")
        .args(["logout", "--account", "work"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("failed to remove enterprise authorization")
    );
    assert_eq!(read_auth_json(home.path())?["OPENAI_API_KEY"], "sk-default");
    assert!(!named_home.join("auth.json").exists());

    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("default-access")
            .account_id("default-workspace")
            .chatgpt_user_id("default-user"),
        AuthCredentialsStoreMode::File,
    )?;
    let default_auth = read_auth_json(home.path())?;
    let output = codex_command(home.path())?
        .current_dir(home.path())
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove("OPENAI_API_KEY")
        .args(["logout", "--account", "work"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("failed to remove enterprise authorization")
    );
    assert_eq!(read_auth_json(home.path())?, default_auth);

    write_chatgpt_auth(
        &named_home,
        ChatGptAuthFixture::new("named-access")
            .account_id("named-workspace")
            .chatgpt_user_id("named-user"),
        AuthCredentialsStoreMode::File,
    )?;
    let output = codex_command(home.path())?
        .current_dir(home.path())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env(
            REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
            format!("{}/oauth/revoke", server.uri()),
        )
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove("OPENAI_API_KEY")
        .args(["logout", "--all"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr)
            .matches("failed to remove enterprise authorization")
            .count(),
        2
    );
    assert!(!home.path().join("auth.json").exists());
    assert!(!named_home.join("auth.json").exists());
    Ok(())
}

#[tokio::test]
async fn logout_uses_verified_pat_scope_without_whoami() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "user@example.com",
            "chatgpt_user_id": "verified-user",
            "chatgpt_account_id": "verified-workspace",
            "chatgpt_plan_type": "enterprise",
            "chatgpt_account_is_fedramp": false,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n",
            server.uri()
        ),
    )?;
    codex_command(home.path())?
        .current_dir(home.path())
        .env("CODEX_AUTHAPI_BASE_URL", server.uri())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["login", "--with-access-token"])
        .write_stdin("at-pat-test\n")
        .assert()
        .success();
    let stored = read_auth_json(home.path())?;
    assert_eq!(stored["ema_scope"]["user_id"], "verified-user");
    assert_eq!(stored["ema_scope"]["workspace_id"], "verified-workspace");
    assert_eq!(stored["ema_scope"]["idp"]["issuer"], "https://idp.example");
    assert_eq!(stored["ema_scope"]["idp"]["client_id"], "enterprise-client");

    // The original IdP profile is needed even after it is removed from configuration.
    write_file_auth_config(home.path())?;
    // A failed lock makes the attempted cleanup visible without touching the host keyring.
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;
    codex_command(home.path())?
        .current_dir(home.path())
        .env("CODEX_AUTHAPI_BASE_URL", "http://127.0.0.1:1")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .assert()
        .success()
        .stderr(contains("failed to remove enterprise authorization"));
    assert!(!home.path().join("auth.json").exists());

    std::fs::remove_file(home.path().join("mcp-oauth-locks"))?;
    let mut replaced_auth = stored;
    replaced_auth["personal_access_token"] = json!("at-another-account");
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_string(&replaced_auth)?,
    )?;
    let output = codex_command(home.path())?
        .current_dir(home.path())
        .env("CODEX_AUTHAPI_BASE_URL", "http://127.0.0.1:1")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .output()?;
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("failed to remove enterprise authorization")
    );
    assert!(!home.path().join("mcp-oauth-locks").exists());
    assert!(!home.path().join("auth.json").exists());
    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn logout_finds_saved_pat_scope_after_file_to_keyring_switch() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "user@example.com",
            "chatgpt_user_id": "verified-user",
            "chatgpt_account_id": "verified-workspace",
            "chatgpt_plan_type": "enterprise",
            "chatgpt_account_is_fedramp": false,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n",
            server.uri()
        ),
    )?;
    codex_command(home.path())?
        .current_dir(home.path())
        .env("CODEX_AUTHAPI_BASE_URL", server.uri())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["login", "--with-access-token"])
        .write_stdin("at-pat-test\n")
        .assert()
        .success();
    assert_eq!(
        read_auth_json(home.path())?["ema_scope"]["user_id"],
        "verified-user"
    );

    std::fs::write(
        home.path().join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n",
    )?;
    // A failed lock exposes attempted grant cleanup without requiring an IdP keyring entry.
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;
    let output = codex_command(home.path())?
        .current_dir(home.path())
        .env("CODEX_AUTHAPI_BASE_URL", "http://127.0.0.1:1")
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .output()?;
    assert_keyring_logout_attempted_enterprise_cleanup(home.path(), &output);
    server.verify().await;
    Ok(())
}

#[test]
fn keyring_logout_infers_legacy_file_scope() -> Result<()> {
    let home = TempDir::new()?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("legacy-access")
            .account_id("legacy-workspace")
            .chatgpt_user_id("legacy-user"),
        AuthCredentialsStoreMode::File,
    )?;
    std::fs::write(
        home.path().join("config.toml"),
        "cli_auth_credentials_store = \"keyring\"\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n",
    )?;
    std::fs::write(home.path().join("mcp-oauth-locks"), "not a directory")?;

    let output = codex_command(home.path())?
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .args(["logout"])
        .output()?;
    assert_keyring_logout_attempted_enterprise_cleanup(home.path(), &output);
    Ok(())
}

#[tokio::test]
async fn logout_uses_stored_account_identity_with_ambient_access_token() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(path("/backend-api/wham/config/bundle"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "config_toml": {"enterprise_managed": []},
            "requirements_toml": {"enterprise_managed": []},
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email": "ambient@example.com",
            "chatgpt_user_id": "ambient-user",
            "chatgpt_account_id": "ambient-workspace",
            "chatgpt_plan_type": "enterprise",
            "chatgpt_account_is_fedramp": false,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let home = TempDir::new()?;
    let named_home = home.path().join("accounts/work");
    std::fs::create_dir_all(&named_home)?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n[mcp_enterprise_managed_auth.idp]\nissuer = \"https://idp.example\"\nclient_id = \"enterprise-client\"\n",
            server.uri()
        ),
    )?;
    let mut expected_lock_files = None;
    for ambient_token in [None, Some("at-ambient")] {
        write_chatgpt_auth(
            &named_home,
            ChatGptAuthFixture::new("named-access")
                .account_id("named-workspace")
                .chatgpt_user_id("named-user"),
            AuthCredentialsStoreMode::File,
        )?;
        let mut command = codex_command(home.path())?;
        command
            .current_dir(home.path())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("CODEX_AUTHAPI_BASE_URL", server.uri())
            .env(
                REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
                format!("{}/oauth/revoke", server.uri()),
            )
            .env_remove("OPENAI_API_KEY")
            .args(["logout", "--account", "work"]);
        if let Some(token) = ambient_token {
            command.env(CODEX_ACCESS_TOKEN_ENV_VAR, token);
        } else {
            command.env_remove(CODEX_ACCESS_TOKEN_ENV_VAR);
        }
        command
            .assert()
            .success()
            .stderr(contains("Successfully logged out"));

        let lock_files: BTreeSet<_> = std::fs::read_dir(home.path().join("mcp-oauth-locks"))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<_>>()?;
        assert!(!lock_files.is_empty());
        if ambient_token.is_none() {
            expected_lock_files = Some(lock_files);
        } else {
            assert_eq!(Some(lock_files), expected_lock_files);
        }
    }
    Ok(())
}

#[test]
fn login_with_access_token_rejects_invalid_jwt() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path())?;
    cmd.args(["login", "--with-access-token"])
        .write_stdin("not-a-jwt\n")
        .assert()
        .failure()
        .stderr(contains("Error logging in with access token"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_prompt_input_follows_authenticated_attribution_setting() -> Result<()> {
    let server = MockServer::start().await;
    let request_count = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/settings/user"))
        .and(header("chatgpt-account-id", "workspace-123"))
        .respond_with(move |_request: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "commit_attribution_enabled": request_count.fetch_add(1, Ordering::SeqCst) == 0,
            }))
        })
        .expect(2)
        .mount(&server)
        .await;
    let codex_home = TempDir::new()?;
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            "cli_auth_credentials_store = \"file\"\nchatgpt_base_url = \"{}/backend-api\"\n",
            server.uri()
        ),
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("workspace-123")
            .plan_type("enterprise"),
        AuthCredentialsStoreMode::File,
    )?;
    for enabled in [true, false] {
        let output = codex_command(codex_home.path())?
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("OPENAI_API_KEY")
            .args(["debug", "prompt-input"])
            .output()?;
        assert!(output.status.success());
        let prompt = String::from_utf8(output.stdout)?;
        assert_eq!(
            prompt.contains("Co-authored-by: Codex <noreply@openai.com>"),
            enabled
        );
        assert!(!prompt.contains("attribution is disabled for the current workspace"));
    }
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_login_revokes_existing_auth_before_requesting_new_tokens() -> Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_auth_id": "device-auth-123",
            "user_code": "CODE-12345",
            "interval": "0",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_code": "authorization-code-123",
            "code_challenge": "code-challenge-123",
            "code_verifier": "code-verifier-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    write_file_auth_config(codex_home.path())?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        serde_json::to_vec(&json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "old-access",
                "refresh_token": "old-refresh",
                "account_id": "old-account",
            },
        }))?,
    )?;

    let issuer = server.uri();
    let mut cmd = codex_command(codex_home.path())?;
    cmd.env(
        REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR,
        format!("{issuer}/oauth/revoke"),
    )
    .env("NO_PROXY", "127.0.0.1,localhost")
    .env("no_proxy", "127.0.0.1,localhost")
    .env_remove("CODEX_ACCESS_TOKEN")
    .env_remove("OPENAI_API_KEY")
    .args(["login", "--device-auth", "--experimental_issuer", &issuer])
    .assert()
    .success()
    .stderr(contains("Successfully logged in"));

    let requests = server
        .received_requests()
        .await
        .context("failed to read mock OAuth requests")?;
    let paths: Vec<&str> = requests.iter().map(|request| request.url.path()).collect();
    assert_eq!(
        paths,
        vec![
            "/oauth/revoke",
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
        ]
    );
    assert_eq!(
        requests[0]
            .body_json::<Value>()
            .context("revoke request should be JSON")?,
        json!({
            "token": "old-refresh",
            "token_type_hint": "refresh_token",
            "client_id": CLIENT_ID,
        })
    );

    let auth = read_auth_json(codex_home.path())?;
    assert_eq!(auth["tokens"]["refresh_token"], "new-refresh");
    Ok(())
}
