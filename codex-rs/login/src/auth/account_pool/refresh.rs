//! Refreshes the credential owner of an in-flight request without changing pool selection.

use super::AccountPoolManager;
use super::AuthManager;
use super::CodexAuth;
use super::RefreshTokenError;

impl AccountPoolManager {
    /// Refreshes the credential owner supplied by a request, without switching pool selection.
    pub(crate) async fn refresh_token_if_unchanged(
        &self,
        expected_auth: &CodexAuth,
    ) -> Result<(), RefreshTokenError> {
        // Pool aliases differ from provider IDs; match the captured credential itself.
        let Some(member) = self
            .pools
            .values()
            .flat_map(|pool| &pool.members)
            .find(|member| {
                AuthManager::auths_equal_for_refresh(
                    member.manager.auth_cached_unpooled().as_ref(),
                    Some(expected_auth),
                )
            })
        else {
            return Ok(());
        };
        member
            .manager
            .refresh_token_if_unchanged(expected_auth)
            .await
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
