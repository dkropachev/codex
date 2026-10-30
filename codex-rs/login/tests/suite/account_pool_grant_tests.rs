use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_login::AccountPoolAuthSelection;
use codex_login::AccountPoolSelectionContext;
use codex_login::AccountPoolUsageBucket;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::load_auth_dot_json;
use codex_login::save_auth;
use codex_login::token_data::AccountOAuthProvenance;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::future::Future;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::account_pool_auth_refresh::AccountPoolTestConfig;

struct Fixture {
    home: TempDir,
    manager: Arc<AuthManager>,
    initial: AuthDotJson,
}

impl Fixture {
    async fn new(server: &MockServer) -> Result<Self> {
        let home = TempDir::new()?;
        let jwt = format!("e30.{}.sig", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({"sub":"user", "https://api.openai.com/auth": {"chatgpt_user_id":"user", "chatgpt_account_id":"provider-id"}}).to_string()
        ));
        let mut initial: AuthDotJson = serde_json::from_value(json!({
            "auth_mode":"chatgpt", "last_refresh":Utc::now(),
            "tokens": {"id_token":jwt, "access_token":"initial-access", "refresh_token":"initial-refresh", "account_id":"provider-id"}
        }))?;
        let tokens = initial.tokens.as_mut().context("tokens")?;
        tokens.login_owner_id = Some("completed-login".into());
        tokens.oauth_provenance = Some(AccountOAuthProvenance {
            issuer: server.uri(),
            client_id: "recorded-client".into(),
        });
        for alias in ["work-pro", "personal-pro"] {
            Self::write(&home.path().join("accounts").join(alias), &initial)?;
        }
        let definition = AccountPoolDefinitionToml {
            provider: "openai".into(),
            policy: AccountPoolPolicyToml::Drain,
            accounts: vec!["work-pro".into(), "personal-pro".into()],
        };
        let config = AccountPoolTestConfig {
            codex_home: home.path().into(),
            chatgpt_base_url: server.uri(),
            account_pool: AccountPoolToml {
                enabled: true,
                default_pool: Some("selected".into()),
                pools: [
                    ("selected".into(), definition.clone()),
                    ("duplicate".into(), definition),
                ]
                .into(),
            },
        };
        let manager =
            AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await?;
        Ok(Self {
            home,
            manager,
            initial,
        })
    }

    fn write(home: &Path, auth: &AuthDotJson) -> Result<()> {
        std::fs::create_dir_all(home)?;
        save_auth(
            home,
            auth,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;
        Ok(())
    }

    fn member_home(&self) -> PathBuf {
        self.home.path().join("accounts/work-pro")
    }

    async fn selection(&self, alias: &str) -> Result<AccountPoolAuthSelection> {
        let mut context =
            AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Spark);
        context.affinity_key = alias.into();
        context.pinned_account_id = Some(alias.into());
        self.manager
            .auth_for_account_pool_selection_context(context)
            .await
            .context("selection")
    }

    async fn reload(&self, selection: &AccountPoolAuthSelection) -> Result<()> {
        self.manager
            .unauthorized_recovery_for_account_pool_selection(selection)
            .context("member recovery")?
            .next()
            .await?;
        Ok(())
    }
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn pooled_grant_uses_selected_persistent_owner() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let ctx = Fixture::new(&server).await?;
    let captured = ctx.manager.auth_cached().context("selected auth")?;
    let selection = ctx.selection("work-pro").await?;
    let root_revision = *ctx.manager.auth_change_state_receiver().borrow();
    assert!(ctx.manager.account_login_is_current(&captured).await?);
    let grant = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("eligible pooled grant")?
        .release();
    assert_eq!(
        grant.tokens(),
        ctx.initial.tokens.as_ref().context("tokens")?
    );
    assert_eq!(grant.current_tokens().await?, ctx.initial.tokens.clone());
    // Root storage is unrelated to the exact configured owner, even with equal member credentials.
    let mut unrelated = ctx.initial.clone();
    unrelated.tokens.as_mut().context("tokens")?.login_owner_id = Some("root-login".into());
    Fixture::write(ctx.home.path(), &unrelated)?;
    assert!(ctx.manager.account_login_is_current(&captured).await?);
    assert_eq!(grant.current_tokens().await?, ctx.initial.tokens.clone());
    let mut rotated = ctx.initial.clone();
    rotated.tokens.as_mut().context("tokens")?.access_token = "rotated-access".into();
    Fixture::write(&ctx.member_home(), &rotated)?;
    ctx.reload(&selection).await?;
    assert!(ctx.manager.account_login_is_current(&captured).await?);
    assert_eq!(grant.current_tokens().await?, rotated.tokens.clone());
    let current = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("rotated grant")?
        .release();
    assert!(grant.same_credential(&current));
    rotated.tokens.as_mut().context("tokens")?.refresh_token = "replacement-refresh".into();
    Fixture::write(&ctx.member_home(), &rotated)?;
    assert!(grant.current_tokens().await?.is_none());
    assert!(ctx.manager.account_login_is_current(&captured).await?);
    rotated.tokens.as_mut().context("tokens")?.login_owner_id = Some("replacement-login".into());
    Fixture::write(&ctx.member_home(), &rotated)?;
    ctx.reload(&selection).await?;
    Fixture::write(&ctx.member_home(), &ctx.initial)?;
    ctx.reload(&selection).await?;
    assert!(
        grant.current_tokens().await?.is_none(),
        "restored bytes cannot restore the member login epoch"
    );
    let lease = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("new login epoch")?;
    assert!(!grant.same_credential(&lease.release()));
    assert_eq!(
        *ctx.manager.auth_change_state_receiver().borrow(),
        root_revision
    );
    let lease = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("before policy change")?;
    ctx.manager
        .set_forced_chatgpt_workspace_id(Some(vec!["different-workspace".into()]));
    assert!(lease.release().current_tokens().await?.is_none());
    assert!(ctx.manager.lease_account_oauth_grant().await?.is_none());
    assert_eq!(
        load_auth_dot_json(
            ctx.home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default()
        )?,
        Some(unrelated)
    );
    Ok(())
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn empty_root_logout_invalidates_only_outstanding_pooled_receipts() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let ctx = Fixture::new(&server).await?;
    let status = ctx.manager.account_pool_status();
    let before = *ctx.manager.auth_change_state_receiver().borrow();
    let lease = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("member-only grant")?;
    assert!(!timeout(Duration::from_secs(5), ctx.manager.logout()).await??);
    assert_eq!(
        *ctx.manager.auth_change_state_receiver().borrow(),
        codex_login::AuthChangeState {
            generation: before.generation + 1,
            ..before
        }
    );
    assert_eq!(ctx.manager.account_pool_status(), status);
    assert_eq!(
        load_auth_dot_json(
            &ctx.member_home(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default()
        )?,
        Some(ctx.initial.clone())
    );
    let grant = lease.release();
    assert!(grant.current_tokens().await?.is_none());
    ctx.manager.reload().await;
    assert!(grant.current_tokens().await?.is_none());
    let fresh = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("member still logged in")?
        .release();
    assert!(!grant.same_credential(&fresh));
    assert_eq!(fresh.current_tokens().await?, ctx.initial.tokens);
    Ok(())
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn pooled_lease_excludes_active_guarded_authority_and_member_recovery() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token":"refreshed-access"})),
        )
        .expect(4)
        .mount(&server)
        .await;
    enum Leaf {
        Active,
        Guarded,
        Authority,
        Recovery,
    }
    for (index, leaf) in [Leaf::Active, Leaf::Guarded, Leaf::Authority, Leaf::Recovery]
        .into_iter()
        .enumerate()
    {
        let ctx = Fixture::new(&server).await?;
        let captured = ctx.manager.auth_cached().context("cached Regular member")?;
        let active = ctx.selection("personal-pro").await?;
        assert_eq!(active.account_id, "personal-pro");
        let mut recovery = ctx
            .manager
            .unauthorized_recovery_for_account_pool_selection(&active)
            .context("member recovery")?;
        let lease = ctx
            .manager
            .lease_account_oauth_grant()
            .await?
            .context("pooled lease")?;
        // Reload remains independent of grant exclusion.
        timeout(Duration::from_secs(5), recovery.next()).await??;
        let status = ctx.manager.account_pool_status();
        let mut refresh: Pin<
            Box<dyn Future<Output = Result<(), codex_login::RefreshTokenError>> + Send>,
        > = match leaf {
            Leaf::Active => Box::pin(ctx.manager.refresh_token()),
            Leaf::Guarded => Box::pin(ctx.manager.refresh_token_if_unchanged(&captured)),
            Leaf::Authority => Box::pin(ctx.manager.refresh_token_from_authority()),
            Leaf::Recovery => Box::pin(async { recovery.next().await.map(|_| ()) }),
        };
        assert!(
            timeout(Duration::from_millis(100), refresh.as_mut())
                .await
                .is_err()
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .context("requests")?
                .iter()
                .filter(|request| request.url.path() == "/oauth/token")
                .count(),
            index
        );
        drop(lease); // Dropping as well as explicit release must free the writer.
        timeout(Duration::from_secs(5), refresh).await??;
        let mut expected = ctx.initial.tokens.clone().context("tokens")?;
        expected.access_token = "refreshed-access".into();
        if index == 0 {
            assert_eq!(
                ctx.manager
                    .auth_cached()
                    .context("cached owner")?
                    .get_token_data()?,
                ctx.initial.tokens.clone().context("tokens")?
            );
            let other = load_auth_dot_json(
                &ctx.home.path().join("accounts/personal-pro"),
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?
            .context("active member storage")?;
            assert_eq!(other.tokens, Some(expected));
        } else if index == 2 {
            assert_eq!(
                ctx.manager
                    .auth_cached()
                    .context("authority owner cache")?
                    .get_token_data()?,
                expected
            );
        }
        assert_eq!(ctx.manager.account_pool_status(), status);
    }
    for request in server
        .received_requests()
        .await
        .context("requests")?
        .into_iter()
        .filter(|request| request.url.path() == "/oauth/token")
    {
        let body: serde_json::Value = request.body_json()?;
        assert_eq!(
            (body["client_id"].clone(), body["refresh_token"].clone()),
            (json!("recorded-client"), json!("initial-refresh"))
        );
    }
    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn detached_pool_refreshes_overlap_and_keep_their_lease_exclusion() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let ctx = Fixture::new(&server).await?;
    let work = ctx.selection("work-pro").await?;
    let personal = ctx.selection("personal-pro").await?;
    let mut expired = ctx.initial.clone();
    expired.last_refresh = Some(Utc::now() - chrono::Duration::days(30));
    for selection in [&work, &personal] {
        expired.tokens.as_mut().context("tokens")?.refresh_token =
            format!("{}-refresh", selection.account_id);
        Fixture::write(
            &ctx.home.path().join("accounts").join(&selection.account_id),
            &expired,
        )?;
        ctx.reload(selection).await?;
    }
    let mut same_member = ctx
        .manager
        .unauthorized_recovery_for_account_pool_selection(&work)
        .context("same member recovery")?;
    same_member.next().await?;
    let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(move |request: &wiremock::Request| {
            let body: serde_json::Value = request.body_json().expect("refresh grant");
            let token = body["refresh_token"].as_str().expect("refresh token");
            let _ = started_tx.send(token.to_owned());
            let delay = if token == "work-pro-refresh" { 3 } else { 5 };
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(delay))
                .set_body_json(json!({"access_token":format!("{token}-access")}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let (usage_tx, mut usage) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |request: &wiremock::Request| {
            let _ = usage_tx.send(
                request.headers["authorization"]
                    .to_str()
                    .expect("authorization")
                    .to_owned(),
            );
            ResponseTemplate::new(200).set_body_json(json!({}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let lease = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("lease before proactive work")?;
    let mut callers = tokio::task::JoinSet::new();
    let manager = Arc::clone(&ctx.manager);
    callers.spawn(async move { manager.refresh_account_pool_usage(Some("selected")).await });
    assert!(
        timeout(Duration::from_millis(100), callers.join_next())
            .await
            .is_err()
    );
    assert!(
        started.try_recv().is_err(),
        "proactive leaves must wait for the writer"
    );
    drop(lease);
    // Both distinct grants start before either delayed OAuth response is available.
    let mut grants = std::collections::BTreeSet::new();
    timeout(Duration::from_secs(2), async {
        for _ in 0..2 {
            grants.insert(started.recv().await.context("OAuth started")?);
        }
        anyhow::Ok(())
    })
    .await??;
    assert_eq!(
        grants,
        [
            "work-pro-refresh".to_string(),
            "personal-pro-refresh".to_string()
        ]
        .into()
    );
    assert!(
        timeout(Duration::from_millis(100), same_member.next())
            .await
            .is_err()
    );
    assert!(
        started.try_recv().is_err(),
        "one member still serializes its refreshes"
    );
    assert_eq!(
        timeout(Duration::from_secs(5), usage.recv())
            .await?
            .context("selected member finished")?,
        "Bearer work-pro-refresh-access"
    );
    // The selected member has released its semaphore; the other worker still owns its reader.
    callers.abort_all();
    while callers.join_next().await.is_some() {}
    let mut waiting_lease = Box::pin(ctx.manager.lease_account_oauth_grant());
    assert!(
        timeout(Duration::from_millis(100), waiting_lease.as_mut())
            .await
            .is_err(),
        "detached workers retain read guards after caller cancellation"
    );
    // Cancel a queued writer, then acquire again; cancellation must not strand the gate.
    drop(waiting_lease);
    let lease = timeout(
        Duration::from_secs(8),
        ctx.manager.lease_account_oauth_grant(),
    )
    .await??
    .context("lease after workers finish")?;
    assert_eq!(lease.tokens().access_token, "work-pro-refresh-access");
    drop(lease);
    assert_eq!(
        timeout(Duration::from_secs(2), usage.recv())
            .await?
            .context("other worker finished")?,
        "Bearer personal-pro-refresh-access"
    );
    assert!(started.try_recv().is_err());
    server.verify().await;
    Ok(())
}

#[serial_test::serial(auth_env)]
#[tokio::test]
async fn pool_usage_deadline_does_not_cancel_pending_credential_refresh() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let ctx = Fixture::new(&server).await?;
    let selected = ctx.selection("work-pro").await?;
    let mut expired = ctx.initial.clone();
    expired.last_refresh = Some(Utc::now() - chrono::Duration::days(30));
    Fixture::write(&ctx.member_home(), &expired)?;
    ctx.reload(&selected).await?;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token":"after-lease"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let lease = ctx
        .manager
        .lease_account_oauth_grant()
        .await?
        .context("pooled grant")?;
    let report = timeout(
        Duration::from_secs(7),
        ctx.manager
            .refresh_account_pool_usage_report(Some("selected")),
    )
    .await?
    .context("refresh report")?;
    assert_eq!(
        (
            report.pools[0].refreshed_count,
            report.pools[0].problems.len()
        ),
        (1, 1)
    );
    let cached = timeout(Duration::from_secs(7), ctx.selection("work-pro")).await??;
    assert_eq!(cached.auth.get_token_data()?.access_token, "initial-access");
    assert!(
        server
            .received_requests()
            .await
            .context("requests")?
            .iter()
            .all(|request| request.url.path() != "/oauth/token")
    );
    drop(lease);
    timeout(Duration::from_secs(5), async {
        loop {
            let stored = load_auth_dot_json(
                &ctx.member_home(),
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )
            .unwrap()
            .unwrap();
            if stored.tokens.unwrap().access_token == "after-lease" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    server.verify().await;
    Ok(())
}
