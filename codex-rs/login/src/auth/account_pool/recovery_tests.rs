use super::super::tests::pool_manager_with_base_url;
use super::super::tests::write_chatgpt_auth;
use super::*;
use codex_config::config_toml::AccountPoolPolicyToml;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

async fn recovery_pool(home: &Path, server: &MockServer) -> AccountPoolManager {
    let mut manager = pool_manager_with_base_url(
        home,
        AccountPoolPolicyToml::Drain,
        vec!["work-pro"],
        Some(server.uri()),
    )
    .await;
    manager
        .pools
        .get_mut("codex-pro")
        .unwrap()
        .definition
        .auto_consume_resets = true;
    manager
}

async fn mount_usage(server: &MockServer, recovered: Arc<AtomicBool>) {
    Mock::given(method("GET")).and(path("/api/codex/usage"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(move |_: &wiremock::Request| {
            let allowed = recovered.load(Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit":{"allowed":allowed,"primary_window":{"used_percent":if allowed {0} else {100}}},
                "rate_limit_reset_credits":{"available_count":2}
            }))
        }).mount(server).await;
}

#[tokio::test]
async fn concurrent_recovery_across_pool_references_redeems_once() {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    let server = MockServer::start().await;
    let recovered = Arc::new(AtomicBool::new(false));
    mount_usage(&server, Arc::clone(&recovered)).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(move |_: &wiremock::Request| {
            recovered.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"code":"reset","windows_reset":2}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let first = recovery_pool(home.path(), &server).await;
    let second = recovery_pool(home.path(), &server).await;
    let context = AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
    let selection_a = first.auth_for_context(context.clone()).await.unwrap();
    let selection_b = second.auth_for_context(context).await.unwrap();
    // Status and selection only read usage; neither spends a reset.
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.method == "POST")
            .count(),
        0
    );
    let mut state_a = AccountPoolRecoveryState::default();
    let mut state_b = AccountPoolRecoveryState::default();
    let (a, b) = tokio::join!(
        first.recover_selection(
            &selection_a,
            AccountPoolUsageBucket::Regular,
            AccountPoolRecoveryMode::SameAccountOnly,
            &mut state_a
        ),
        second.recover_selection(
            &selection_b,
            AccountPoolUsageBucket::Regular,
            AccountPoolRecoveryMode::SameAccountOnly,
            &mut state_b
        ),
    );
    assert_eq!((a, b), (true, true));
    assert_eq!(
        first.status().unwrap().members[0].regular_remaining,
        Some(100)
    );
    server.verify().await;
}

#[tokio::test]
async fn uncertain_redemption_reuses_id_and_is_bounded() {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    let server = MockServer::start().await;
    mount_usage(&server, Arc::new(AtomicBool::new(false))).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"code":"reset","windows_reset":2}))
                .set_delay(Duration::from_secs(6)),
        )
        .up_to_n_times(/*n*/ 1)
        .with_priority(/*p*/ 1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let manager = recovery_pool(home.path(), &server).await;
    let selection = manager
        .auth_for_context(AccountPoolSelectionContext::default_for_bucket(
            AccountPoolUsageBucket::Regular,
        ))
        .await
        .unwrap();
    for _ in 0..3 {
        assert!(
            !manager
                .recover_selection(
                    &selection,
                    AccountPoolUsageBucket::Regular,
                    AccountPoolRecoveryMode::SameAccountOnly,
                    &mut AccountPoolRecoveryState::default()
                )
                .await
        );
    }
    let posts: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method == "POST")
        .map(|request| request.body_json::<serde_json::Value>().unwrap())
        .collect();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0], posts[1]);
    assert!(manager.pools["codex-pro"].is_exhausted("work-pro", AccountPoolUsageBucket::Regular));
}

#[tokio::test]
async fn ineffective_reset_and_unknown_refresh_do_not_clear_exhaustion() {
    for response in [
        json!({"code":"reset","windows_reset":2}),
        json!({"code":"nothing_to_reset"}),
        json!({"code":"no_credit"}),
        json!({"code":"already_redeemed"}),
    ] {
        let home = tempfile::tempdir().unwrap();
        write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
        let server = MockServer::start().await;
        mount_usage(&server, Arc::new(AtomicBool::new(false))).await;
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
        let manager = recovery_pool(home.path(), &server).await;
        let selection = manager
            .auth_for_context(AccountPoolSelectionContext::default_for_bucket(
                AccountPoolUsageBucket::Regular,
            ))
            .await
            .unwrap();
        for _ in 0..2 {
            assert!(
                !manager
                    .recover_selection(
                        &selection,
                        AccountPoolUsageBucket::Regular,
                        AccountPoolRecoveryMode::SameAccountOnly,
                        &mut AccountPoolRecoveryState::default()
                    )
                    .await
            );
        }
        let pool = &manager.pools["codex-pro"];
        pool.apply_usage("work-pro", usage::parse_usage(&json!({})).unwrap());
        assert!(pool.is_exhausted("work-pro", AccountPoolUsageBucket::Regular));
        server.verify().await;
    }
}

#[tokio::test]
async fn partial_refresh_preserves_cached_resources_and_bounds_slow_members() {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(home.path(), "personal-pro", "personal@example.com");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"rate_limit":{"allowed":true},"rate_limit_reset_credits":{"available_count":4}}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("ChatGPT-Account-ID", "personal-pro"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({}))
                .set_delay(Duration::from_secs(6)),
        )
        .mount(&server)
        .await;
    let manager = pool_manager_with_base_url(
        home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
        Some(server.uri()),
    )
    .await;
    let pool = &manager.pools["codex-pro"];
    pool.apply_usage("personal-pro", usage::parse_usage(&json!({"rate_limit":{"allowed":false},"credits":{"has_credits":true,"unlimited":false,"balance":"12.3400"},"rate_limit_reset_credits":{"available_count":2}})).unwrap());
    let report = manager.refresh_usage_with_report(/*pool_id*/ None).await;
    assert_eq!(
        (
            report.pools[0].refreshed_count,
            report.pools[0].problems.len()
        ),
        (1, 1)
    );
    let status = manager.status().unwrap();
    assert_eq!(
        status
            .members
            .iter()
            .map(|member| (
                member.rate_limit_reset_credit_count,
                member
                    .credits
                    .as_ref()
                    .and_then(|credits| credits.balance.clone())
            ))
            .collect::<Vec<_>>(),
        vec![(Some(4), None), (Some(2), Some("12.3400".to_string()))]
    );
    assert_eq!(status.active_account_id, None);
}

#[tokio::test]
async fn refreshed_included_quota_remains_eligible_after_an_earlier_error() {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(home.path(), "personal-pro", "personal@example.com");
    let manager = pool_manager_with_base_url(
        home.path(),
        AccountPoolPolicyToml::Drain,
        vec!["work-pro", "personal-pro"],
        /*chatgpt_base_url*/ None,
    )
    .await;
    let context = AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
    let work = manager.auth_for_context(context.clone()).await.unwrap();
    let mut state = AccountPoolRecoveryState::default();
    assert!(
        manager
            .recover_selection(
                &work,
                AccountPoolUsageBucket::Regular,
                AccountPoolRecoveryMode::AllowSwitch,
                &mut state
            )
            .await
    );
    let personal = manager.auth_for_context(context.clone()).await.unwrap();
    assert_eq!(personal.account_id, "personal-pro");
    manager.pools["codex-pro"].apply_usage(
        "work-pro",
        usage::parse_usage(&json!({"rate_limit":{"allowed":true}})).unwrap(),
    );
    assert!(
        manager
            .recover_selection(
                &personal,
                AccountPoolUsageBucket::Regular,
                AccountPoolRecoveryMode::AllowSwitch,
                &mut state
            )
            .await
    );
    assert_eq!(
        manager.auth_for_context(context).await.unwrap().account_id,
        "work-pro"
    );
}

#[tokio::test]
async fn passive_confirmation_rearms_resets_after_an_ineffective_redemption() {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    let server = MockServer::start().await;
    let included = Arc::new(AtomicBool::new(false));
    mount_usage(&server, Arc::clone(&included)).await;
    let redemption_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let redeemed = Arc::clone(&redemption_count);
    let redemption_included = Arc::clone(&included);
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(move |_: &wiremock::Request| {
            if redeemed.fetch_add(1, Ordering::SeqCst) > 0 {
                redemption_included.store(true, Ordering::SeqCst);
            }
            ResponseTemplate::new(200).set_body_json(json!({"code":"reset","windows_reset":2}))
        })
        .expect(2)
        .mount(&server)
        .await;
    let manager = recovery_pool(home.path(), &server).await;
    let context = AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
    let first = manager.auth_for_context(context.clone()).await.unwrap();
    assert!(
        !manager
            .recover_selection(
                &first,
                AccountPoolUsageBucket::Regular,
                AccountPoolRecoveryMode::AllowSwitch,
                &mut AccountPoolRecoveryState::default()
            )
            .await
    );
    included.store(true, Ordering::SeqCst);
    manager.refresh_usage(/*pool_id*/ None).await;
    let later = manager.auth_for_context(context).await.unwrap();
    included.store(false, Ordering::SeqCst);
    assert!(
        manager
            .recover_selection(
                &later,
                AccountPoolUsageBucket::Regular,
                AccountPoolRecoveryMode::AllowSwitch,
                &mut AccountPoolRecoveryState::default()
            )
            .await
    );
    server.verify().await;
}
