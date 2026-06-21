use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use chrono::Duration;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use codex_login::load_auth_dot_json;
use codex_login::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const INITIAL_ACCESS_TOKEN: &str = "initial-access-token";
const INITIAL_REFRESH_TOKEN: &str = "initial-refresh-token";

#[serial_test::serial(auth_refresh)]
#[tokio::test]
async fn refresh_token_uses_active_account_pool_member() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let codex_home = TempDir::new()?;
    let endpoint = format!("{}/oauth/token", server.uri());
    let _env_guard = EnvGuard::set(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, endpoint);

    let account_id = "work-pro";
    let account_home = codex_home.path().join("accounts").join(account_id);
    std::fs::create_dir_all(&account_home)?;
    let initial_last_refresh = Utc::now() - Duration::days(1);
    let initial_tokens = TokenData {
        id_token: IdTokenInfo {
            raw_jwt: minimal_jwt(),
            ..Default::default()
        },
        access_token: INITIAL_ACCESS_TOKEN.to_string(),
        refresh_token: INITIAL_REFRESH_TOKEN.to_string(),
        account_id: Some(account_id.to_string()),
    };
    let initial_auth = AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(initial_tokens.clone()),
        last_refresh: Some(initial_last_refresh),
        agent_identity: None,
        personal_access_token: None,
    };
    save_auth(&account_home, &initial_auth, AuthCredentialsStoreMode::File)?;

    let config = AccountPoolTestConfig {
        codex_home: codex_home.path().to_path_buf(),
        chatgpt_base_url: server.uri(),
        account_pool: AccountPoolToml {
            enabled: true,
            default_pool: Some("codex-pro".to_string()),
            pools: [(
                "codex-pro".to_string(),
                AccountPoolDefinitionToml {
                    provider: "openai".to_string(),
                    policy: AccountPoolPolicyToml::Drain,
                    accounts: vec![account_id.to_string()],
                },
            )]
            .into(),
        },
    };
    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;
    auth_manager.auth().await.context("pool auth should load")?;

    auth_manager
        .refresh_token()
        .await
        .context("pooled refresh should succeed")?;

    let stored = load_auth_dot_json(&account_home, AuthCredentialsStoreMode::File)?
        .context("member auth.json should exist")?;
    let refreshed_tokens = TokenData {
        access_token: "new-access-token".to_string(),
        refresh_token: "new-refresh-token".to_string(),
        ..initial_tokens.clone()
    };
    let tokens = stored.tokens.as_ref().context("tokens should exist")?;
    assert_eq!(tokens, &refreshed_tokens);
    let refreshed_at = stored
        .last_refresh
        .as_ref()
        .context("last_refresh should be recorded")?;
    assert!(
        *refreshed_at >= initial_last_refresh,
        "last_refresh should advance"
    );

    server.verify().await;
    Ok(())
}

struct AccountPoolTestConfig {
    codex_home: PathBuf,
    chatgpt_base_url: String,
    account_pool: AccountPoolToml,
}

impl AuthManagerConfig for AccountPoolTestConfig {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        AuthCredentialsStoreMode::File
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        None
    }

    fn chatgpt_base_url(&self) -> String {
        self.chatgpt_base_url.clone()
    }

    fn account_pool(&self) -> Option<AccountPoolToml> {
        Some(self.account_pool.clone())
    }
}

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: String) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: these tests execute serially, so updating the process environment is safe.
        unsafe {
            std::env::set_var(key, &value);
        }
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: the guard restores the original environment value before other tests run.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn jwt_with_payload(payload: serde_json::Value) -> String {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };

    fn b64(data: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    }

    let header_bytes = match serde_json::to_vec(&header) {
        Ok(bytes) => bytes,
        Err(err) => panic!("serialize header: {err}"),
    };
    let payload_bytes = match serde_json::to_vec(&payload) {
        Ok(bytes) => bytes,
        Err(err) => panic!("serialize payload: {err}"),
    };
    let header_b64 = b64(&header_bytes);
    let payload_b64 = b64(&payload_bytes);
    let signature_b64 = b64(b"sig");
    format!("{header_b64}.{payload_b64}.{signature_b64}")
}

fn minimal_jwt() -> String {
    jwt_with_payload(json!({ "sub": "user-123" }))
}
