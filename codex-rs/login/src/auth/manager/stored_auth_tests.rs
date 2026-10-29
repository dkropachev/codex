use super::super::AgentIdentityAuthRecord;
use super::super::AgentIdentityStorage;
use super::super::AuthDotJson;
use super::*;
use crate::auth::storage::FileAuthStorage;
use anyhow::Context;
use base64::Engine;
use codex_protocol::account::PlanType;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

fn external_auth(account_id: &str) -> anyhow::Result<AuthDotJson> {
    let payload = json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": account_id,
        "chatgpt_plan_type": "pro",
    }});
    let payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload)?);
    Ok(AuthDotJson::from_external_access_token(
        &format!("e30.{payload}.c2ln"),
        account_id,
        Some("pro"),
    )?)
}

fn managed_auth(account_id: &str) -> anyhow::Result<AuthDotJson> {
    let mut auth = external_auth(account_id)?;
    auth.auth_mode = Some(AuthMode::Chatgpt);
    auth.tokens
        .as_mut()
        .context("fixture should contain tokens")?
        .refresh_token = format!("refresh-{account_id}");
    Ok(auth)
}

#[tokio::test]
async fn stored_constructor_prefers_home_ephemeral_and_honors_ephemeral_only() -> anyhow::Result<()>
{
    let home = tempdir()?;
    let route = crate::test_support::transport_default_auth_route_config();
    let ephemeral = create_auth_storage(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    );
    let file = FileAuthStorage::new(home.path().to_path_buf());
    let ephemeral_auth = managed_auth("ephemeral-account")?;
    let file_auth = managed_auth("file-account")?;
    ephemeral.save(&ephemeral_auth)?;
    file.save(&file_auth)?;

    for mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let auth = CodexAuth::from_stored_auth_storage(
            home.path(),
            mode,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            &route,
        )
        .await?
        .context("ephemeral auth should load")?;
        assert_eq!(
            (auth.auth_mode(), auth.get_account_id()),
            (AuthMode::Chatgpt, Some("ephemeral-account".to_string()))
        );
        let CodexAuth::Chatgpt(managed) = auth else {
            panic!("expected managed ChatGPT auth");
        };
        assert_eq!(managed.storage.load()?, Some(ephemeral_auth.clone()));
    }

    ephemeral.delete()?;
    assert!(
        CodexAuth::from_stored_auth_storage(
            home.path(),
            AuthCredentialsStoreMode::Ephemeral,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            &route,
        )
        .await?
        .is_none()
    );
    let auth = CodexAuth::from_stored_auth_storage(
        home.path(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        &route,
    )
    .await?
    .context("persistent auth should load")?;
    assert_eq!(
        (auth.auth_mode(), auth.get_account_id()),
        (AuthMode::Chatgpt, Some("file-account".to_string()))
    );
    let CodexAuth::Chatgpt(managed) = auth else {
        panic!("expected managed ChatGPT auth");
    };
    assert_eq!(managed.storage.load()?, Some(file_auth));

    let external_auth = external_auth("external-account")?;
    file.save(&external_auth)?;
    let external = CodexAuth::from_stored_auth_storage(
        home.path(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        &route,
    )
    .await?
    .context("external tokens should retain their auth mode in persistent storage")?;
    assert_eq!(
        (external.auth_mode(), external.get_current_token_data()),
        (AuthMode::Chatgpt, external_auth.tokens)
    );
    Ok(())
}

#[tokio::test]
async fn stored_constructor_propagates_ephemeral_hydration_errors() -> anyhow::Result<()> {
    let home = tempdir()?;
    let file = FileAuthStorage::new(home.path().to_path_buf());
    file.save(&managed_auth("file-account")?)?;
    let ephemeral = create_auth_storage(
        home.path().to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    );
    let mut invalid = managed_auth("invalid-account")?;
    invalid.auth_mode = Some(AuthMode::ApiKey);
    ephemeral.save(&invalid)?;
    let error = CodexAuth::from_stored_auth_storage(
        home.path(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .err()
    .context("an invalid ephemeral credential must not fall back to the file")?;
    ephemeral.delete()?;
    assert_eq!(error.to_string(), "API key auth is missing a key.");
    Ok(())
}

#[derive(Debug)]
struct UnavailableStorage;

impl AuthStorageBackend for UnavailableStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "store unavailable",
        ))
    }

    fn save(&self, _auth: &AuthDotJson) -> std::io::Result<()> {
        unreachable!("loading must not save credentials")
    }

    fn delete(&self) -> std::io::Result<bool> {
        unreachable!("loading must not delete credentials")
    }
}

#[tokio::test]
async fn shared_storage_hydration_preserves_errors_and_restriction_order() -> anyhow::Result<()> {
    let home = tempdir()?;
    let route = crate::test_support::transport_default_auth_route_config();
    let allowed_methods = [ForcedLoginMethod::Api];
    let workspaces = ["allowed-workspace".to_string()];
    let mut context = AuthStorageContext {
        codex_home: home.path(),
        allowed_login_methods: Some(&allowed_methods),
        forced_chatgpt_workspace_id: Some(&workspaces),
        chatgpt_base_url: None,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        agent_identity_authapi_base_url: None,
        auth_route_config: &route,
    };
    let error = context
        .load(&UnavailableStorage, AuthCredentialsStoreMode::File)
        .await
        .err()
        .context("storage errors should propagate")?;
    assert_eq!(
        (error.kind(), error.to_string()),
        (
            std::io::ErrorKind::PermissionDenied,
            "store unavailable".to_string()
        )
    );

    let file = FileAuthStorage::new(home.path().to_path_buf());
    assert!(
        context
            .load(&file, AuthCredentialsStoreMode::File)
            .await?
            .is_none()
    );
    let mut agent = managed_auth("unused-account")?;
    agent.auth_mode = Some(AuthMode::AgentIdentity);
    agent.agent_identity = Some(AgentIdentityStorage::Record(AgentIdentityAuthRecord {
        agent_runtime_id: "runtime".to_string(),
        agent_private_key: "invalid-private-key".to_string(),
        account_id: "other-workspace".to_string(),
        chatgpt_user_id: "user".to_string(),
        email: None,
        plan_type: PlanType::Pro,
        chatgpt_account_is_fedramp: false,
        task_id: None,
    }));
    file.save(&agent)?;
    assert!(
        context
            .load(&file, AuthCredentialsStoreMode::File)
            .await?
            .is_none()
    );

    context.allowed_login_methods = None;
    let error = context
        .load(&file, AuthCredentialsStoreMode::File)
        .await
        .err()
        .context("workspace validation must precede Agent Identity hydration")?;
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    Ok(())
}
