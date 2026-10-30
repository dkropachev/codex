//! Resource routing through the agent's public turn API.
use super::*;
use codex_config::config_toml::AccountPoolFallbackPriorityToml;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryScenario {
    IncludedThenReset,
    Included,
    ResetsFirst,
    CreditsFirst,
    ResetsDisabled,
    CreditsDisabled,
    Disabled,
}

fn resource_definition() -> AccountPoolDefinitionToml {
    AccountPoolDefinitionToml {
        provider: "openai".to_string(),
        policy: AccountPoolPolicyToml::Drain,
        accounts: vec!["work-pro".to_string(), "personal-pro".to_string()],
        auto_consume_resets: true,
        auto_consume_credits: true,
        fallback_priority: AccountPoolFallbackPriorityToml::ResetsFirst,
    }
}

async fn build_resource_codex(
    server: &MockServer,
    definition: AccountPoolDefinitionToml,
) -> Result<core_test_support::test_codex::TestCodex> {
    let base_url = server.uri();
    test_codex()
        .with_config_auth_manager()
        .with_pre_build_hook(|home| {
            write_chatgpt_auth(home, "work-pro", "work@example.com");
            write_chatgpt_auth(home, "personal-pro", "personal@example.com");
        })
        .with_config(move |config| {
            config.chatgpt_base_url = base_url;
            config.account_pool = Some(AccountPoolToml {
                enabled: true,
                default_pool: Some("codex-pro".to_string()),
                pools: [("codex-pro".to_string(), definition)].into(),
            });
        })
        .build_with_auto_env(server)
        .await
}

#[test_case::test_case(RecoveryScenario::IncludedThenReset; "deferred reset remains available after other quota is exhausted")]
#[test_case::test_case(RecoveryScenario::Included; "remaining included quota precedes both fallbacks")]
#[test_case::test_case(RecoveryScenario::ResetsFirst; "resets before credits")]
#[test_case::test_case(RecoveryScenario::CreditsFirst; "credits before resets")]
#[test_case::test_case(RecoveryScenario::ResetsDisabled; "disabled resets are skipped")]
#[test_case::test_case(RecoveryScenario::CreditsDisabled; "disabled credits are skipped")]
#[test_case::test_case(RecoveryScenario::Disabled; "disabled fallbacks surface original error")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_resource_recovery(scenario: RecoveryScenario) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let failed = Arc::new(AtomicBool::new(false));
    let restored = Arc::new(AtomicBool::new(false));
    let usage_failed = Arc::clone(&failed);
    let usage_restored = Arc::clone(&restored);
    Mock::given(method("GET")).and(path("/api/codex/usage"))
        .respond_with(move |request: &wiremock::Request| {
            let work = request.headers["ChatGPT-Account-ID"] == "work-pro";
            let allowed = if work { !usage_failed.load(Ordering::SeqCst) || usage_restored.load(Ordering::SeqCst) } else { matches!(scenario, RecoveryScenario::Included | RecoveryScenario::IncludedThenReset) };
            ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit":{"allowed":allowed,"primary_window":{"used_percent":if allowed {0} else {100}}},
                "rate_limit_reset_credits":{"available_count":if work {2} else {0}},
                "credits":{"has_credits":!work,"unlimited":false,"balance":if work {"0"} else {"12.3400"}}
            }))
        }).mount(&server).await;
    let should_reset = matches!(
        scenario,
        RecoveryScenario::ResetsFirst
            | RecoveryScenario::CreditsDisabled
            | RecoveryScenario::IncludedThenReset
    );
    let reset_restored = Arc::clone(&restored);
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(move |_: &wiremock::Request| {
            reset_restored.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"code":"reset","windows_reset":2}))
        })
        .expect(usize::from(should_reset) as u64)
        .mount(&server)
        .await;
    let response_failed = Arc::clone(&failed);
    let response_restored = Arc::clone(&restored);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |request: &wiremock::Request| {
            if request.headers["ChatGPT-Account-ID"] == "work-pro"
                && !response_restored.load(Ordering::SeqCst)
            {
                response_failed.store(true, Ordering::SeqCst);
                return usage_limit_response("codex", /*window_minutes*/ 300);
            }
            if scenario == RecoveryScenario::IncludedThenReset
                && request.headers["ChatGPT-Account-ID"] == "personal-pro"
            {
                return usage_limit_response("codex", /*window_minutes*/ 300);
            }
            responses::sse_response(sse(vec![
                ev_assistant_message("done", "recovered"),
                ev_completed("done"),
            ]))
        })
        .expect(if scenario == RecoveryScenario::Disabled {
            1
        } else if scenario == RecoveryScenario::IncludedThenReset {
            3
        } else {
            2
        })
        .mount(&server)
        .await;
    let mut definition = resource_definition();
    definition.auto_consume_resets = !matches!(
        scenario,
        RecoveryScenario::ResetsDisabled | RecoveryScenario::Disabled
    );
    definition.auto_consume_credits = !matches!(
        scenario,
        RecoveryScenario::CreditsDisabled | RecoveryScenario::Disabled
    );
    if scenario == RecoveryScenario::CreditsFirst {
        definition.fallback_priority = AccountPoolFallbackPriorityToml::CreditsFirst;
    }
    let test = build_resource_codex(&server, definition).await?;
    submit_prompt(&test.codex, "recover pool resources").await?;
    if scenario == RecoveryScenario::Disabled {
        assert!(
            wait_for_error(&test.codex)
                .await
                .to_lowercase()
                .contains("usage limit")
        );
    }
    wait_for_turn_complete(&test.codex).await;
    let requests = responses::received_responses_requests(&server).await;
    let accounts = requests
        .iter()
        .map(|request| {
            request
                .header("ChatGPT-Account-ID")
                .expect("pool request account header")
        })
        .collect::<Vec<_>>();
    let expected = if scenario == RecoveryScenario::IncludedThenReset {
        vec!["work-pro", "personal-pro", "work-pro"]
    } else if scenario == RecoveryScenario::Disabled {
        vec!["work-pro"]
    } else if should_reset {
        vec!["work-pro", "work-pro"]
    } else {
        vec!["work-pro", "personal-pro"]
    };
    assert_eq!(accounts, expected);
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_permitted_capacity_does_not_use_other_authentication() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit":{"allowed":false}, "rate_limit_reset_credits":{"available_count":2},
            "credits":{"has_credits":true,"unlimited":false,"balance":"10"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let mut definition = resource_definition();
    definition.auto_consume_resets = false;
    definition.auto_consume_credits = false;
    let test = build_resource_codex(&server, definition).await?;
    submit_prompt(&test.codex, "no permitted pool capacity").await?;
    let error = wait_for_error(&test.codex).await;
    assert!(error.contains("no permitted pool capacity"), "{error}");
    wait_for_turn_complete(&test.codex).await;
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hot_cache_can_reset_same_account_below_switch_threshold() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let requests = Arc::new(AtomicUsize::new(0));
    let restored = Arc::new(AtomicBool::new(false));
    let usage_requests = Arc::clone(&requests);
    let usage_restored = Arc::clone(&restored);
    Mock::given(method("GET")).and(path("/api/codex/usage"))
        .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit":{"allowed":usage_requests.load(Ordering::SeqCst) < 2 || usage_restored.load(Ordering::SeqCst)},
            "rate_limit_reset_credits":{"available_count":2}
        }))).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(move |_: &wiremock::Request| {
            restored.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"code":"reset","windows_reset":2}))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(move |_: &wiremock::Request| {
            if requests.fetch_add(1, Ordering::SeqCst) == 1 {
                usage_limit_response_with_reset(
                    "codex", /*window_minutes*/ 15, /*reset_after_seconds*/ 300,
                )
            } else {
                responses::sse_response(sse(vec![
                    ev_assistant_message("done", "done"),
                    ev_completed_with_cached_tokens("done"),
                ]))
            }
        })
        .expect(3)
        .mount(&server)
        .await;
    let mut definition = resource_definition();
    definition.accounts = vec!["work-pro".to_string()];
    let test = build_resource_codex(&server, definition).await?;
    submit_prompt(&test.codex, "warm the cache").await?;
    wait_for_turn_complete(&test.codex).await;
    submit_prompt(&test.codex, "retry same account with reset").await?;
    wait_for_turn_complete(&test.codex).await;
    server.verify().await;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum UnsafeRecovery {
    VisibleOutput,
    UnsupportedError,
}

#[test_case::test_case(UnsafeRecovery::VisibleOutput; "visible output prevents reset")]
#[test_case::test_case(UnsafeRecovery::UnsupportedError; "usage not included prevents reset")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_resets_respect_output_safety(boundary: UnsafeRecovery) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"rate_limit":{"allowed":false},"rate_limit_reset_credits":{"available_count":2}})))
        .mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let mut events = vec![ev_response_created("unsafe")];
    let code = match boundary {
        UnsafeRecovery::VisibleOutput => {
            events.push(ev_assistant_message("visible", "visible output"));
            "usage_limit_reached"
        }
        UnsafeRecovery::UnsupportedError => "usage_not_included",
    };
    events.push(json!({"type":"response.failed","response":{"id":"unsafe","error":{
        "type":code,"code":code,"message":"usage error","resets_at":Utc::now().timestamp()+3600,"plan_type":"pro"
    }}}));
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            responses::sse_response(sse(events))
                .insert_header("x-codex-active-limit", "codex")
                .insert_header("x-codex-primary-used-percent", "100.0")
                .insert_header("x-codex-primary-window-minutes", "300"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let test = build_resource_codex(&server, resource_definition()).await?;
    submit_prompt(&test.codex, "do not redeem after unsafe failure").await?;
    wait_for_error(&test.codex).await;
    wait_for_turn_complete(&test.codex).await;
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ineffective_model_recovery_is_bounded_even_when_usage_reports_success() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let restored = Arc::new(AtomicBool::new(false));
    let usage_restored = Arc::clone(&restored);
    Mock::given(method("GET")).and(path("/api/codex/usage"))
        .respond_with(move |_: &wiremock::Request| ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit":{"allowed":usage_restored.load(Ordering::SeqCst)}, "rate_limit_reset_credits":{"available_count":10}
        }))).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(move |_: &wiremock::Request| {
            restored.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"code":"reset","windows_reset":2}))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(usage_limit_response("codex", /*window_minutes*/ 300))
        .expect(2)
        .mount(&server)
        .await;
    let mut definition = resource_definition();
    definition.accounts = vec!["work-pro".to_string()];
    let test = build_resource_codex(&server, definition).await?;
    submit_prompt(&test.codex, "bound failed reset recovery").await?;
    assert!(
        wait_for_error(&test.codex)
            .await
            .to_lowercase()
            .contains("usage limit")
    );
    wait_for_turn_complete(&test.codex).await;
    server.verify().await;
    Ok(())
}
