//! Shared stored-credential hydration and per-home loading without credential overrides.

use std::path::Path;

use super::AuthCredentialsStoreMode;
use super::AuthKeyringBackendKind;
use super::AuthRouteConfig;
use super::AuthStorageBackend;
use super::CodexAuth;
use super::ForcedLoginMethod;
use super::agent_identity_authapi_base_url;
use super::auth_mode_is_allowed;
use super::create_auth_storage;
use super::ensure_agent_identity_workspace_allowed;
use super::ensure_auth_workspace_allowed;

pub(super) struct AuthStorageContext<'a> {
    pub(super) codex_home: &'a Path,
    pub(super) allowed_login_methods: Option<&'a [ForcedLoginMethod]>,
    pub(super) forced_chatgpt_workspace_id: Option<&'a [String]>,
    pub(super) chatgpt_base_url: Option<&'a str>,
    pub(super) keyring_backend_kind: AuthKeyringBackendKind,
    pub(super) agent_identity_authapi_base_url: Option<&'a str>,
    pub(super) auth_route_config: &'a AuthRouteConfig,
}

impl AuthStorageContext<'_> {
    pub(super) async fn load(
        &self,
        storage: &dyn AuthStorageBackend,
        source_mode: AuthCredentialsStoreMode,
    ) -> std::io::Result<Option<CodexAuth>> {
        let Some(auth_dot_json) = storage.load()? else {
            return Ok(None);
        };
        if !auth_mode_is_allowed(self.allowed_login_methods, auth_dot_json.resolved_mode()) {
            return Ok(None);
        }
        if let Some(agent_identity) = auth_dot_json.agent_identity.as_ref() {
            ensure_agent_identity_workspace_allowed(
                self.forced_chatgpt_workspace_id,
                agent_identity,
            )?;
        }
        let auth = CodexAuth::from_auth_dot_json(
            self.codex_home,
            auth_dot_json,
            source_mode,
            self.chatgpt_base_url,
            self.keyring_backend_kind,
            self.agent_identity_authapi_base_url,
            self.auth_route_config,
        )
        .await?;
        if let CodexAuth::PersonalAccessToken(auth) = &auth {
            ensure_auth_workspace_allowed(self.forced_chatgpt_workspace_id, auth.account_id())?;
        }
        Ok(Some(auth))
    }
}

impl CodexAuth {
    /// Loads this home's ephemeral credentials first, then its configured persistent store.
    /// Explicit ephemeral storage never falls back to persistent credentials. `CODEX_API_KEY` and
    /// `CODEX_ACCESS_TOKEN` do not override credential selection; stored credentials retain the
    /// usual hydration and routing behavior.
    pub async fn from_stored_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Option<Self>> {
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url).ok();
        let context = AuthStorageContext {
            codex_home,
            allowed_login_methods: None,
            forced_chatgpt_workspace_id: None,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url: agent_identity_authapi_base_url.as_deref(),
            auth_route_config,
        };
        let ephemeral_storage = create_auth_storage(
            codex_home.to_path_buf(),
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
        if let Some(auth) = context
            .load(
                ephemeral_storage.as_ref(),
                AuthCredentialsStoreMode::Ephemeral,
            )
            .await?
        {
            return Ok(Some(auth));
        }
        if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
            return Ok(None);
        }
        let storage = create_auth_storage(
            codex_home.to_path_buf(),
            auth_credentials_store_mode,
            keyring_backend_kind,
        );
        context
            .load(storage.as_ref(), auth_credentials_store_mode)
            .await
    }
}

#[cfg(test)]
#[path = "stored_auth_tests.rs"]
mod tests;
