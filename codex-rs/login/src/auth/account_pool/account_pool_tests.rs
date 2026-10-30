use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AuthKeyringBackendKind;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

#[tokio::test]
async fn drain_selects_first_available_subscription_account() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;

    let auth = pool.auth().await.expect("pool should select auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
    assert_eq!(pool.status(), Some(active_work_pool_status()));
}

#[tokio::test]
async fn missing_member_does_not_invalidate_pool() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;

    let auth = pool.auth().await.expect("pool should select fallback auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("personal@example.com")
    );
    let status = pool.status().expect("status");
    assert_eq!(
        status
            .members
            .iter()
            .find(|member| member.account_id == "work-pro")
            .and_then(|member| member.unavailable_reason.as_deref()),
        Some("missing credentials")
    );
}

#[tokio::test]
async fn cached_auth_does_not_activate_pool_member() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;

    let auth = pool.auth_cached().expect("pool should select cached auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
    assert_eq!(
        pool.status().expect("status").active_account_id.as_deref(),
        None
    );
}

#[tokio::test]
async fn explicit_cached_activation_selects_pool_member() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;

    let auth = pool
        .activate_cached_auth()
        .expect("pool should activate cached auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
    assert_eq!(pool.status(), Some(active_work_pool_status()));
}

#[tokio::test]
async fn managed_policy_filters_disallowed_pool_member_auth() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    let managed_auth_policy = codex_config::ManagedAuthPolicy {
        allowed_login_methods: Some(vec![ForcedLoginMethod::Api]),
        ..Default::default()
    };
    let pool = AccountPoolManager::from_config(
        AccountPoolToml {
            enabled: true,
            default_pool: Some("codex-pro".to_string()),
            pools: [(
                "codex-pro".to_string(),
                AccountPoolDefinitionToml {
                    provider: "openai".to_string(),
                    policy: AccountPoolPolicyToml::Drain,
                    auto_consume_resets: false,
                    auto_consume_credits: false,
                    fallback_priority: Default::default(),
                    accounts: vec!["work-pro".to_string()],
                },
            )]
            .into(),
        },
        AuthConfig {
            codex_home: codex_home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy,
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        },
    )
    .await
    .expect("account pool should be enabled");

    assert_eq!(pool.auth().await, None);
}

#[tokio::test]
async fn load_balance_selects_largest_fresh_remaining_bucket() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let regular_pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    regular_pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
    regular_pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

    let regular = regular_pool
        .auth_for_bucket(AccountPoolUsageBucket::Regular)
        .await
        .expect("regular auth");
    assert_eq!(
        regular.get_account_email().as_deref(),
        Some("personal@example.com")
    );

    let spark_pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    spark_pool.set_usage_for_testing("work-pro", Some(10), Some(100), Instant::now());
    spark_pool.set_usage_for_testing("personal-pro", Some(90), Some(1), Instant::now());

    let spark = spark_pool
        .auth_for_bucket(AccountPoolUsageBucket::Spark)
        .await
        .expect("spark auth");
    assert_eq!(
        spark.get_account_email().as_deref(),
        Some("work@example.com")
    );
}

#[tokio::test]
async fn load_balance_keeps_active_member_when_available() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    pool.set_usage_for_testing("work-pro", Some(50), Some(50), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());

    let account_pool = pool.pools.get("codex-pro").expect("pool");
    let affinity_key = "thread-hot";
    account_pool.set_assignment(
        account_pool.assignment_key(AccountPoolUsageBucket::Regular, affinity_key),
        "work-pro".to_string(),
        Some(AccountPoolCacheHint {
            input_tokens: 20_000,
            cached_input_tokens: 12_000,
        }),
    );

    let auth = pool
        .auth_for_context(AccountPoolSelectionContext {
            bucket: AccountPoolUsageBucket::Regular,
            affinity_key: affinity_key.to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: Some(AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 12_000,
            }),
            pinned_account_id: None,
        })
        .await
        .expect("regular auth");
    assert_eq!(
        auth.auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
}

#[tokio::test]
async fn load_balance_rebalances_warm_affinity_when_another_member_is_healthier() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    pool.set_usage_for_testing("work-pro", Some(5), Some(5), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(90), Some(90), Instant::now());
    let account_pool = pool.pools.get("codex-pro").expect("pool");
    let affinity_key = "thread-warm";
    let cache_hint = AccountPoolCacheHint {
        input_tokens: 10_000,
        cached_input_tokens: 3_000,
    };
    account_pool.set_assignment(
        account_pool.assignment_key(AccountPoolUsageBucket::Regular, affinity_key),
        "work-pro".to_string(),
        Some(cache_hint),
    );

    let auth = pool
        .auth_for_context(AccountPoolSelectionContext {
            bucket: AccountPoolUsageBucket::Regular,
            affinity_key: affinity_key.to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: Some(cache_hint),
            pinned_account_id: None,
        })
        .await
        .expect("regular auth");

    assert_eq!(
        auth.auth.get_account_email().as_deref(),
        Some("personal@example.com")
    );
}

#[tokio::test]
async fn load_balance_stores_assignments_by_affinity_and_usage_bucket() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    pool.set_usage_for_testing("work-pro", Some(90), Some(1), Instant::now());
    pool.set_usage_for_testing("personal-pro", Some(1), Some(90), Instant::now());

    let regular = pool
        .auth_for_context(AccountPoolSelectionContext {
            bucket: AccountPoolUsageBucket::Regular,
            affinity_key: "thread-a".to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: None,
            pinned_account_id: None,
        })
        .await
        .expect("regular auth");
    let spark = pool
        .auth_for_context(AccountPoolSelectionContext {
            bucket: AccountPoolUsageBucket::Spark,
            affinity_key: "thread-a".to_string(),
            operation_kind: AccountPoolOperationKind::Stream,
            cache_hint: None,
            pinned_account_id: None,
        })
        .await
        .expect("spark auth");

    assert_eq!(
        regular.auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
    assert_eq!(
        spark.auth.get_account_email().as_deref(),
        Some("personal@example.com")
    );
    let account_pool = pool.pools.get("codex-pro").expect("pool");
    assert_eq!(
        account_pool
            .assignment(&account_pool.assignment_key(AccountPoolUsageBucket::Regular, "thread-a"))
            .expect("regular assignment")
            .account_id,
        "work-pro"
    );
    assert_eq!(
        account_pool
            .assignment(&account_pool.assignment_key(AccountPoolUsageBucket::Spark, "thread-a"))
            .expect("spark assignment")
            .account_id,
        "personal-pro"
    );
}

#[tokio::test]
async fn load_balance_throttles_stale_usage_refresh_attempts() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(500))
        .expect(2)
        .mount(&server)
        .await;

    let pool = pool_manager_with_base_url(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
        Some(server.uri()),
    )
    .await;

    let first_auth = pool
        .auth_for_bucket(AccountPoolUsageBucket::Regular)
        .await
        .expect("first auth should still select credentials");
    let second_auth = pool
        .auth_for_bucket(AccountPoolUsageBucket::Regular)
        .await
        .expect("second auth should still select credentials");

    assert_eq!(
        first_auth.get_account_email(),
        second_auth.get_account_email()
    );
    server.verify().await;
}

#[tokio::test]
async fn refresh_usage_requests_pool_members_in_parallel() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let server = MockServer::start().await;
    let request_count = Arc::new(AtomicUsize::new(0));
    let responder_request_count = Arc::clone(&request_count);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |_request: &wiremock::Request| {
            responder_request_count.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "rate_limit": {
                        "primary_window": {
                            "used_percent": 25.0
                        }
                    }
                }))
                .set_delay(Duration::from_millis(/*millis*/ 2_000))
        })
        .expect(2)
        .mount(&server)
        .await;

    let pool = pool_manager_with_base_url(
        codex_home.path(),
        AccountPoolPolicyToml::LoadBalance,
        vec!["work-pro", "personal-pro"],
        Some(server.uri()),
    )
    .await;

    let observed_request_count = Arc::clone(&request_count);
    let ((), request_count_while_first_response_delayed) =
        tokio::join!(pool.refresh_usage(/*pool_id*/ None), async move {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 1_000)).await;
            observed_request_count.load(Ordering::SeqCst)
        });
    assert_eq!(
        request_count_while_first_response_delayed, 2,
        "usage refresh should start all member requests before the first response returns"
    );

    assert_eq!(
        pool.status(),
        Some(AccountPoolStatus {
            pool_id: "codex-pro".to_string(),
            policy: AccountPoolPolicyToml::LoadBalance,
            active_account_id: None,
            members: vec![
                AccountPoolMemberStatus {
                    account_id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(PlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: Some(75),
                    spark_remaining: None,
                    credits: None,
                    rate_limit_reset_credit_count: None,
                    last_error: None,
                },
                AccountPoolMemberStatus {
                    account_id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(PlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: Some(75),
                    spark_remaining: None,
                    credits: None,
                    rate_limit_reset_credit_count: None,
                    last_error: None,
                },
            ],
        })
    );
}

#[tokio::test]
async fn exhausted_member_is_skipped_for_that_bucket() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    pool.mark_exhausted(
        "work-pro",
        AccountPoolUsageBucket::Regular,
        "usage limit reached".to_string(),
    );

    let auth = pool
        .auth()
        .await
        .expect("pool should select non-exhausted auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("personal@example.com")
    );
}

#[tokio::test]
async fn refreshed_positive_usage_makes_exhausted_member_available_again() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    write_chatgpt_auth(codex_home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(codex_home.path(), "personal-pro", "personal@example.com");

    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    pool.mark_exhausted(
        "work-pro",
        AccountPoolUsageBucket::Regular,
        "usage limit reached".to_string(),
    );
    pool.set_usage_for_testing(
        "work-pro",
        Some(100),
        /*spark_remaining*/ None,
        Instant::now(),
    );

    let auth = pool
        .auth()
        .await
        .expect("pool should select refreshed auth");
    assert_eq!(
        auth.get_account_email().as_deref(),
        Some("work@example.com")
    );
}

#[tokio::test]
async fn mark_active_exhausted_reports_whether_bucket_has_usable_fallback() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let pool = pool_manager(
        codex_home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
    )
    .await;
    let account_pool = pool.pools.get("codex-pro").expect("pool");
    account_pool.set_assignment(
        account_pool.assignment_key(
            AccountPoolUsageBucket::Regular,
            DEFAULT_ACCOUNT_POOL_AFFINITY_KEY,
        ),
        "work-pro".to_string(),
        /*cache_hint*/ None,
    );

    assert!(!pool.mark_active_exhausted(
        AccountPoolUsageBucket::Regular,
        "usage limit reached".to_string()
    ));
    assert_eq!(
        pool.status(),
        Some(AccountPoolStatus {
            pool_id: "codex-pro".to_string(),
            policy: AccountPoolPolicyToml::Drain,
            active_account_id: Some("work-pro".to_string()),
            members: vec![
                AccountPoolMemberStatus {
                    account_id: "work-pro".to_string(),
                    email: None,
                    plan_type: None,
                    active: true,
                    unavailable_reason: Some("usage limit reached".to_string()),
                    regular_remaining: None,
                    spark_remaining: None,
                    credits: None,
                    rate_limit_reset_credit_count: None,
                    last_error: Some("usage limit reached".to_string()),
                },
                AccountPoolMemberStatus {
                    account_id: "personal-pro".to_string(),
                    email: None,
                    plan_type: None,
                    active: false,
                    unavailable_reason: Some("missing credentials".to_string()),
                    regular_remaining: None,
                    spark_remaining: None,
                    credits: None,
                    rate_limit_reset_credit_count: None,
                    last_error: None,
                },
            ],
        })
    );
}

#[test]
fn remaining_from_rate_limit_maps_used_percent_to_remaining_percent() {
    assert_eq!(
        remaining_from_rate_limit(Some(&json!({
            "primary_window": { "used_percent": 12.4 }
        }))),
        Some(88)
    );
    assert_eq!(
        remaining_from_rate_limit(Some(&json!({
            "primary_window": { "used_percent": 150.0 }
        }))),
        Some(0)
    );
    assert_eq!(
        remaining_from_rate_limit(Some(&json!({
            "primary_window": { "used_percent": -7.0 }
        }))),
        Some(100)
    );
    assert_eq!(
        remaining_from_rate_limit(Some(&json!({
            "primary_window": { "used_percent": 0.0 },
            "secondary_window": { "used_percent": 14.0 }
        }))),
        Some(86)
    );
    assert_eq!(remaining_from_rate_limit(Some(&json!({}))), None);
}

pub(super) async fn pool_manager(
    codex_home: &Path,
    policy: AccountPoolPolicyToml,
    accounts: Vec<&str>,
) -> AccountPoolManager {
    pool_manager_with_base_url(codex_home, policy, accounts, /*chatgpt_base_url*/ None).await
}

pub(super) async fn pool_manager_with_base_url(
    codex_home: &Path,
    policy: AccountPoolPolicyToml,
    accounts: Vec<&str>,
    chatgpt_base_url: Option<String>,
) -> AccountPoolManager {
    AccountPoolManager::from_config(
        AccountPoolToml {
            enabled: true,
            default_pool: Some("codex-pro".to_string()),
            pools: [(
                "codex-pro".to_string(),
                AccountPoolDefinitionToml {
                    provider: "openai".to_string(),
                    policy,
                    auto_consume_resets: false,
                    auto_consume_credits: false,
                    fallback_priority: Default::default(),
                    accounts: accounts.into_iter().map(str::to_string).collect(),
                },
            )]
            .into(),
        },
        AuthConfig {
            codex_home: codex_home.to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_login_method: None,
            chatgpt_base_url,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy: Default::default(),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        },
    )
    .await
    .expect("account pool should be enabled")
}

fn active_work_pool_status() -> AccountPoolStatus {
    AccountPoolStatus {
        pool_id: "codex-pro".to_string(),
        policy: AccountPoolPolicyToml::Drain,
        active_account_id: Some("work-pro".to_string()),
        members: vec![
            AccountPoolMemberStatus {
                account_id: "work-pro".to_string(),
                email: Some("work@example.com".to_string()),
                plan_type: Some(PlanType::Pro),
                active: true,
                unavailable_reason: None,
                regular_remaining: None,
                spark_remaining: None,
                credits: None,
                rate_limit_reset_credit_count: None,
                last_error: None,
            },
            AccountPoolMemberStatus {
                account_id: "personal-pro".to_string(),
                email: Some("personal@example.com".to_string()),
                plan_type: Some(PlanType::Pro),
                active: false,
                unavailable_reason: None,
                regular_remaining: None,
                spark_remaining: None,
                credits: None,
                rate_limit_reset_credit_count: None,
                last_error: None,
            },
        ],
    }
}

pub(super) fn write_chatgpt_auth(codex_home: &Path, account_id: &str, email: &str) {
    let account_home = codex_home.join("accounts").join(account_id);
    fs::create_dir_all(&account_home).expect("create account home");
    let jwt = fake_jwt(json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": "pro",
            "chatgpt_account_id": account_id,
            "user_id": format!("user-{account_id}")
        }
    }));
    let auth = json!({
        "auth_mode": AuthMode::Chatgpt,
        "tokens": {
            "id_token": jwt,
            "access_token": fake_jwt(json!({ "exp": Utc::now().timestamp() + 3600 })),
            "refresh_token": format!("refresh-{account_id}"),
            "account_id": account_id,
        },
        "last_refresh": Utc::now(),
    });
    fs::write(
        account_home.join("auth.json"),
        serde_json::to_string_pretty(&auth).expect("serialize auth"),
    )
    .expect("write auth");
}

fn fake_jwt(payload: serde_json::Value) -> String {
    let header = json!({"alg": "none"});
    let encode = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&value).expect("serialize jwt part"))
    };
    format!("{}.{}.sig", encode(header), encode(payload))
}
