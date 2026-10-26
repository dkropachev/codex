//! Scoped access to a persisted login grant. Refresh exclusion lasts only through the IdP
//! exchange; callers must recheck ownership before publishing its result. Storage reads
//! observe other processes at these boundaries, not continuously.

use sha2::Digest;
use sha2::Sha256;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::OwnedSemaphorePermit;

use super::AuthCredentialsStoreMode;
use super::AuthManager;
use super::AuthMode;
use super::CodexAuth;
use super::auth_storage_commit_guard;
use super::create_auth_storage;
use super::validate_auth_restrictions;
use crate::token_data::TokenData;

/// A snapshot of one account grant, checked against both cached ownership and storage.
/// This does not authorize a destination; consumers must validate the recorded issuer/client.
pub struct AccountOAuthGrant {
    pub(super) manager: Arc<AuthManager>,
    owner: Arc<AuthManager>,
    owner_login_generation: u64,
    invalidation_generation: u64,
    pub(super) tokens: TokenData,
    pub(super) login_generation: u64,
}

/// Identity of a refresh grant, excluding access and ID token rotation.
/// This opaque digest compares revisions; it does not authorize a destination.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountOAuthGrantRevision([u8; 32]);

impl From<&TokenData> for AccountOAuthGrantRevision {
    fn from(tokens: &TokenData) -> Self {
        let mut hash = Sha256::new();
        let provenance = tokens.oauth_provenance.as_ref();
        for value in [
            tokens.login_owner_id.as_deref(),
            tokens.account_id.as_deref(),
            tokens.id_token.chatgpt_user_id.as_deref(),
            provenance.map(|provenance| provenance.issuer.as_str()),
            provenance.map(|provenance| provenance.client_id.as_str()),
            Some(tokens.refresh_token.as_str()),
        ] {
            let value = value.unwrap_or_default();
            hash.update(value.len().to_be_bytes());
            hash.update(value.as_bytes());
        }
        Self(hash.finalize().into())
    }
}

impl fmt::Debug for AccountOAuthGrantRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountOAuthGrantRevision")
            .finish_non_exhaustive()
    }
}

/// Keeps ordinary refresh from rotating the account token during an IdP exchange.
/// Login and logout remain available and invalidate the grant independently of this lease.
pub struct AccountOAuthGrantLease {
    pub(super) grant: AccountOAuthGrant,
    owner_permit: Option<OwnedSemaphorePermit>,
    pub(super) permit: OwnedSemaphorePermit,
    coordination_permit: tokio::sync::OwnedRwLockWriteGuard<()>,
}

impl fmt::Debug for AccountOAuthGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountOAuthGrant").finish_non_exhaustive()
    }
}

impl fmt::Debug for AccountOAuthGrantLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountOAuthGrantLease")
            .finish_non_exhaustive()
    }
}

impl AccountOAuthGrantLease {
    pub fn tokens(&self) -> &TokenData {
        &self.grant.tokens
    }

    /// Release refresh exclusion before contacting a resource authorization server.
    pub fn release(self) -> AccountOAuthGrant {
        drop(self.owner_permit);
        drop(self.permit);
        drop(self.coordination_permit);
        self.grant
    }
}

impl AccountOAuthGrant {
    pub fn tokens(&self) -> &TokenData {
        &self.tokens
    }

    /// Compare credential revisions without exposing the refresh token to state holders.
    pub fn same_credential(&self, other: &Self) -> bool {
        self.login_generation == other.login_generation
            && self.invalidation_generation == other.invalidation_generation
            && Arc::ptr_eq(&self.owner, &other.owner)
            && self.owner_login_generation == other.owner_login_generation
            && AccountOAuthGrantRevision::from(&self.tokens)
                == AccountOAuthGrantRevision::from(&other.tokens)
    }

    pub(super) fn owner_is_current(&self) -> bool {
        if self.manager.account_pool.as_ref().is_some_and(|pool| {
            pool.cached_auth_with_owner()
                .is_none_or(|(_, owner)| !Arc::ptr_eq(&owner, &self.owner))
        }) {
            return false;
        }
        let Ok(cached) = self.owner.inner.read() else {
            return false;
        };
        self.manager
            .account_grant_invalidation
            .load(Ordering::SeqCst)
            == self.invalidation_generation
            && self.manager.auth_change_state_tx.borrow().login_generation == self.login_generation
            && self.owner.auth_change_state_tx.borrow().login_generation
                == self.owner_login_generation
            && cached
                .auth
                .as_ref()
                .filter(|auth| matches!(auth, super::CodexAuth::Chatgpt(_)))
                .and_then(|auth| auth.get_token_data().ok())
                .is_some_and(|tokens| same_account_grant(&self.tokens, &tokens))
    }

    /// Read the still-current grant after asynchronous work. Consumers must validate
    /// the latest ID token's eligibility before accepting an exchange result.
    /// A storage failure is an error, never proof that the saved grant is still valid.
    pub async fn current_tokens(&self) -> io::Result<Option<TokenData>> {
        if !self.owner_is_current() {
            return Ok(None);
        }
        let stored = self.manager.read_account_grant_tokens().await?;
        Ok(stored.filter(|tokens| {
            self.owner_is_current()
                && AccountOAuthGrantRevision::from(&self.tokens)
                    == AccountOAuthGrantRevision::from(tokens)
        }))
    }
}

pub(super) fn same_account_grant(a: &TokenData, b: &TokenData) -> bool {
    a.oauth_provenance == b.oauth_provenance && same_account_login(a, b)
}

pub(super) fn same_account_login(a: &TokenData, b: &TokenData) -> bool {
    a.login_owner_id == b.login_owner_id
        && a.account_id == b.account_id
        && a.id_token.chatgpt_user_id == b.id_token.chatgpt_user_id
}

impl AuthManager {
    /// Borrows a persisted account grant while excluding this manager's token refreshes.
    /// Release the lease after the IdP exchange, then check the grant before using its result.
    pub async fn lease_account_oauth_grant(
        self: &Arc<Self>,
    ) -> std::io::Result<Option<AccountOAuthGrantLease>> {
        // The writer excludes every member, including active, proactive and detached refreshes.
        let coordination_permit = Arc::clone(&self.pool_refresh_gate).write_owned().await;
        let permit = Arc::clone(&self.refresh_lock)
            .acquire_owned()
            .await
            .map_err(|_| std::io::Error::other("Account refresh coordination is unavailable."))?;
        let owner = if !self.has_external_auth()
            && let Some(pool) = &self.account_pool
        {
            let Some((_, owner)) = pool.cached_auth_with_owner() else {
                return Ok(None);
            };
            owner
        } else {
            Arc::clone(self)
        };
        let owner_permit = if Arc::ptr_eq(self, &owner) {
            None
        } else {
            Some(
                Arc::clone(&owner.refresh_lock)
                    .acquire_owned()
                    .await
                    .map_err(|_| {
                        std::io::Error::other("Account refresh coordination is unavailable.")
                    })?,
            )
        };
        let login_generation = self.auth_change_state_tx.borrow().login_generation;
        let owner_login_generation = owner.auth_change_state_tx.borrow().login_generation;
        let invalidation_generation = self.account_grant_invalidation.load(Ordering::SeqCst);
        let Some(tokens) = self.read_account_grant_tokens().await? else {
            return Ok(None);
        };
        let grant = AccountOAuthGrant {
            manager: Arc::clone(self),
            owner,
            owner_login_generation,
            invalidation_generation,
            tokens,
            login_generation,
        };
        if !grant.owner_is_current() {
            return Ok(None);
        }
        Ok(Some(AccountOAuthGrantLease {
            grant,
            owner_permit,
            permit,
            coordination_permit,
        }))
    }

    /// Checks the persisted account owner without requiring an EMA-eligible account grant.
    /// Dedicated enterprise credentials still belong to this account login.
    pub async fn account_login_is_current(&self, expected: &CodexAuth) -> std::io::Result<bool> {
        if !matches!(expected, CodexAuth::Chatgpt(_)) {
            return Ok(self.auth_cached().is_some_and(|current| {
                !matches!(current, CodexAuth::Chatgpt(_))
                    && current.get_account_id() == expected.get_account_id()
                    && current.get_chatgpt_user_id() == expected.get_chatgpt_user_id()
            }));
        }
        let expected = expected.get_token_data()?;
        if expected
            .login_owner_id
            .as_ref()
            .is_none_or(String::is_empty)
        {
            return Ok(false);
        }
        let matches_owner = || {
            self.auth_cached().is_some_and(|auth| {
                matches!(auth, CodexAuth::Chatgpt(_))
                    && auth
                        .get_token_data()
                        .is_ok_and(|tokens| same_account_login(&expected, &tokens))
            })
        };
        if !matches_owner() {
            return Ok(false);
        }
        let stored = self.read_persisted_account_tokens().await?;
        Ok(matches_owner()
            && stored
                .as_ref()
                .is_some_and(|tokens| same_account_login(&expected, tokens)))
    }

    async fn read_persisted_account_tokens(&self) -> std::io::Result<Option<TokenData>> {
        let owner = if !self.has_external_auth()
            && let Some(pool) = &self.account_pool
        {
            let Some((_, owner)) = pool.cached_auth_with_owner() else {
                return Ok(None);
            };
            Some(owner)
        } else {
            None
        };
        let owner = owner.as_deref().unwrap_or(self);
        let home = owner.codex_home.clone();
        let mode = owner.auth_credentials_store_mode;
        let backend = owner.keyring_backend_kind;
        let originator = codex_otel::auth_storage::AuthStorageOriginator::current();
        let stored = tokio::task::spawn_blocking(move || {
            originator.sync_scope(|| {
                let _commit = auth_storage_commit_guard()?;
                create_auth_storage(home, mode, backend).load_account_grant()
            })
        })
        .await
        .map_err(|_| std::io::Error::other("Account credential read failed."))?
        .map_err(|_: std::io::Error| {
            std::io::Error::other("Account credential storage is unavailable.")
        })?;
        Ok(stored
            .filter(|auth| auth.resolved_mode() == AuthMode::Chatgpt)
            .and_then(|auth| auth.tokens))
    }

    async fn read_account_grant_tokens(&self) -> std::io::Result<Option<TokenData>> {
        if self.has_external_auth()
            || self.auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral
        {
            return Ok(None);
        }
        let Some(auth @ CodexAuth::Chatgpt(_)) = self.auth_cached() else {
            return Ok(None);
        };
        let auth_is_allowed = || {
            validate_auth_restrictions(
                Some(&self.allowed_login_methods()),
                self.effective_chatgpt_workspaces().as_deref(),
                &auth,
            )
            .is_ok()
        };
        if !auth_is_allowed() {
            return Ok(None);
        }
        let cached = auth.get_token_data()?;
        let stored = self.read_persisted_account_tokens().await?;
        if self.has_external_auth()
            || !auth_is_allowed()
            || self.auth_cached().is_none_or(|current| {
                !current
                    .get_token_data()
                    .is_ok_and(|tokens| same_account_grant(&cached, &tokens))
            })
        {
            return Ok(None);
        }
        Ok(stored.filter(|tokens| {
            tokens.oauth_provenance.as_ref().is_some_and(|provenance| {
                !provenance.issuer.trim().is_empty() && !provenance.client_id.trim().is_empty()
            }) && tokens
                .login_owner_id
                .as_ref()
                .is_some_and(|id| !id.is_empty())
                && !tokens.refresh_token.is_empty()
                && tokens
                    .id_token
                    .chatgpt_user_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
                && tokens.account_id.as_ref().is_some_and(|id| !id.is_empty())
                && same_account_grant(tokens, &cached)
        }))
    }
}
