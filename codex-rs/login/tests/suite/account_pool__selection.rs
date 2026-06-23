use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountPoolCacheHint;
use codex_login::AccountPoolOperationKind;
use codex_login::AccountPoolSelectionContext;
use codex_login::AccountPoolUsageBucket;
use codex_login::AuthDotJson;
use codex_login::auth::AccountPoolManager;
use codex_login::save_auth;
use codex_login::token_data::IdTokenInfo;
use codex_login::token_data::TokenData;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn cached_auth_read_does_not_activate_or_switch_pool_members() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    let pool = load_balance_pool(codex_home.path()).await?;

    let auth = pool.auth_cached().context("cached auth should exist")?;

    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
    let status = pool.status().context("pool status should exist")?;
    assert_eq!(status.active_account_id, None);
    assert_eq!(
        status
            .members
            .into_iter()
            .map(|member| (member.account_id, member.active))
            .collect::<Vec<_>>(),
        vec![
            ("work-pro".to_string(), false),
            ("personal-pro".to_string(), false),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn cold_load_balance_selection_chooses_healthiest_remaining_quota() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    let pool = load_balance_pool(codex_home.path()).await?;
    pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

    let selection = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            "cold-thread",
        ))
        .await
        .context("selection should exist")?;

    assert_eq!(selection.account_id, "personal-pro");
    Ok(())
}

#[tokio::test]
async fn hot_affinity_keeps_assigned_account() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    let pool = load_balance_pool(codex_home.path()).await?;
    pool.set_usage_for_testing("work-pro", Some(90), Some(90), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(50), Some(50), Instant::now());
    let affinity_key = "hot-thread";

    let initial = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            affinity_key,
        ))
        .await
        .context("initial selection should exist")?;
    assert_eq!(initial.account_id, "work-pro");

    pool.record_cache_hint(
        AccountPoolUsageBucket::Regular,
        affinity_key.to_string(),
        AccountPoolCacheHint {
            input_tokens: 20_000,
            cached_input_tokens: 12_000,
        },
    );
    pool.set_usage_for_testing("work-pro", Some(50), Some(50), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());

    let hot = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            affinity_key,
        ))
        .await
        .context("hot selection should exist")?;

    assert_eq!(hot.account_id, "work-pro");
    Ok(())
}

#[tokio::test]
async fn warm_affinity_rebalances_when_another_member_is_materially_healthier() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    let pool = load_balance_pool(codex_home.path()).await?;
    pool.set_usage_for_testing("work-pro", Some(90), Some(90), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(50), Some(50), Instant::now());
    let affinity_key = "warm-thread";

    let initial = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            affinity_key,
        ))
        .await
        .context("initial selection should exist")?;
    assert_eq!(initial.account_id, "work-pro");

    pool.record_cache_hint(
        AccountPoolUsageBucket::Regular,
        affinity_key.to_string(),
        AccountPoolCacheHint {
            input_tokens: 10_000,
            cached_input_tokens: 3_000,
        },
    );
    pool.set_usage_for_testing("work-pro", Some(5), Some(5), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());

    let warm = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            affinity_key,
        ))
        .await
        .context("warm selection should exist")?;

    assert_eq!(warm.account_id, "personal-pro");
    Ok(())
}

#[tokio::test]
async fn assignments_are_separate_by_affinity_key_and_usage_bucket() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    let pool = load_balance_pool(codex_home.path()).await?;
    pool.set_usage_for_testing("work-pro", Some(90), Some(1), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(1), Some(90), Instant::now());

    let regular_a = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            "thread-a",
        ))
        .await
        .context("regular thread-a selection should exist")?;
    let spark_a = pool
        .auth_for_context(selection_context(AccountPoolUsageBucket::Spark, "thread-a"))
        .await
        .context("spark thread-a selection should exist")?;
    assert_eq!(regular_a.account_id, "work-pro");
    assert_eq!(spark_a.account_id, "personal-pro");

    pool.record_cache_hint(
        AccountPoolUsageBucket::Regular,
        "thread-a".to_string(),
        AccountPoolCacheHint {
            input_tokens: 20_000,
            cached_input_tokens: 12_000,
        },
    );
    pool.set_usage_for_testing("work-pro", Some(50), Some(1), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());

    let regular_b = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            "thread-b",
        ))
        .await
        .context("regular thread-b selection should exist")?;
    let regular_a_again = pool
        .auth_for_context(selection_context(
            AccountPoolUsageBucket::Regular,
            "thread-a",
        ))
        .await
        .context("regular thread-a should keep assignment")?;

    assert_eq!(regular_b.account_id, "personal-pro");
    assert_eq!(regular_a_again.account_id, "work-pro");
    Ok(())
}

async fn load_balance_pool(codex_home: &Path) -> Result<AccountPoolManager> {
    AccountPoolManager::from_config(
        codex_home,
        AccountPoolToml {
            enabled: true,
            default_pool: Some("codex-pro".to_string()),
            pools: [(
                "codex-pro".to_string(),
                AccountPoolDefinitionToml {
                    provider: "openai".to_string(),
                    policy: AccountPoolPolicyToml::LoadBalance,
                    accounts: vec!["work-pro".to_string(), "personal-pro".to_string()],
                },
            )]
            .into(),
        },
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
    )
    .await
    .context("account pool should be enabled")
}

fn selection_context(
    bucket: AccountPoolUsageBucket,
    affinity_key: &str,
) -> AccountPoolSelectionContext {
    AccountPoolSelectionContext {
        bucket,
        affinity_key: affinity_key.to_string(),
        operation_kind: AccountPoolOperationKind::Stream,
        cache_hint: None,
        pinned_account_id: None,
    }
}

fn write_chatgpt_auth(codex_home: &Path, account_id: &str, email: &str) -> Result<()> {
    let account_home = codex_home.join("accounts").join(account_id);
    fs::create_dir_all(&account_home)?;
    let tokens = TokenData {
        id_token: IdTokenInfo {
            raw_jwt: fake_jwt(json!({
                "email": email,
                "https://api.openai.com/auth": {
                    "chatgpt_plan_type": "pro",
                    "chatgpt_account_id": account_id,
                    "user_id": format!("user-{account_id}")
                }
            }))?,
            ..Default::default()
        },
        access_token: fake_jwt(json!({ "exp": Utc::now().timestamp() + 3600 }))?,
        refresh_token: format!("refresh-{account_id}"),
        account_id: Some(account_id.to_string()),
    };
    save_auth(
        &account_home,
        &AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
        },
        AuthCredentialsStoreMode::File,
    )?;
    Ok(())
}

fn fake_jwt(payload: serde_json::Value) -> Result<String> {
    let header = json!({"alg": "none"});
    let encode = |value: serde_json::Value| -> Result<String> {
        let bytes = serde_json::to_vec(&value)?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    };
    Ok(format!("{}.{}.sig", encode(header)?, encode(payload)?))
}
