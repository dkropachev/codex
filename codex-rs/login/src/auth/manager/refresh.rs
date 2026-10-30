//! Refresh selected credential snapshots without selecting another credential source.

use std::sync::Arc;
use std::sync::Mutex;

use super::AuthDotJson;
use super::AuthManager;
use super::ChatgptAuth;
use super::CodexAuth;
use super::RefreshReason;
use super::RefreshTokenError;
use super::TokenData;
use super::oauth_client_id;
use super::persist_tokens;
use super::refresh_token_endpoint;
use super::request_chatgpt_token_refresh;

impl CodexAuth {
    /// Proactively refreshes managed ChatGPT credentials in their selected store when needed.
    /// Other auth modes remain unchanged. This updates only this snapshot and never selects
    /// environment credentials or a pool member; persistence rejects a changed login.
    pub async fn refresh_if_needed(&mut self) -> Result<(), RefreshTokenError> {
        let Some(reason) = AuthManager::proactive_refresh_reason(self) else {
            return Ok(());
        };
        if let Self::Chatgpt(auth) = self {
            let tokens = auth
                .current_token_data()
                .ok_or_else(|| std::io::Error::other("Token data is not available."))?;
            let updated = auth.refresh(tokens, reason).await?;
            auth.state.auth_dot_json = Arc::new(Mutex::new(Some(updated)));
        }
        Ok(())
    }
}

impl ChatgptAuth {
    pub(super) async fn refresh(
        &self,
        tokens: TokenData,
        reason: RefreshReason,
    ) -> Result<AuthDotJson, RefreshTokenError> {
        let endpoint = refresh_token_endpoint(tokens.oauth_provenance.as_ref());
        let client_id = tokens
            .oauth_provenance
            .as_ref()
            .map(|provenance| provenance.client_id.clone())
            .unwrap_or_else(oauth_client_id);
        let refresh_response = request_chatgpt_token_refresh(
            tokens.refresh_token.clone(),
            &endpoint,
            &client_id,
            self.client(),
            reason,
        )
        .await?;

        // An explicit override may send refresh to another authority. Retain provenance
        // only when the endpoint used by this request matches the recorded issuer.
        let provenance = tokens.oauth_provenance.clone().filter(|provenance| {
            endpoint == format!("{}/oauth/token", provenance.issuer.trim_end_matches('/'))
        });

        persist_tokens(
            self.storage(),
            &tokens,
            provenance,
            refresh_response.id_token,
            refresh_response.access_token,
            refresh_response.refresh_token,
        )
        .map_err(RefreshTokenError::from)
    }
}
