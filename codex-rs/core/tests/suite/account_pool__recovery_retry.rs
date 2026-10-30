//! Uncertain reset redemption remains retryable through the public turn API.
use super::*;
use pretty_assertions::assert_eq;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryOutcome {
    Recovered,
    StillUnavailable,
}

#[test_case::test_case(RetryOutcome::Recovered; "later turn reuses uncertain request ID")]
#[test_case::test_case(RetryOutcome::StillUnavailable; "uncertain attempts stop after two posts")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_reset_can_retry_on_a_later_turn(outcome: RetryOutcome) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let restored = Arc::new(AtomicBool::new(false));
    let usage_restored = Arc::clone(&restored);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit": {"allowed": usage_restored.load(Ordering::SeqCst)},
                "rate_limit_reset_credits": {"available_count": 2}
            }))
        })
        .mount(&server)
        .await;
    let resets = Arc::new(AtomicUsize::new(0));
    let reset_attempts = Arc::clone(&resets);
    let reset_restored = Arc::clone(&restored);
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(move |_: &wiremock::Request| {
            let attempt = reset_attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 1 && outcome == RetryOutcome::Recovered {
                reset_restored.store(true, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_json(json!({"code": "reset"}))
            } else {
                ResponseTemplate::new(503)
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let response_restored = Arc::clone(&restored);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_: &wiremock::Request| {
            if response_restored.load(Ordering::SeqCst) {
                responses::sse_response(sse(vec![
                    ev_response_created("done"),
                    ev_assistant_message("message", "recovered"),
                    ev_completed("done"),
                ]))
            } else {
                usage_limit_response("codex", /*window_minutes*/ 300)
            }
        })
        .expect(match outcome {
            RetryOutcome::Recovered => 3,
            RetryOutcome::StillUnavailable => 2,
        })
        .mount(&server)
        .await;
    let mut definition = resource_definition();
    definition.accounts = vec!["work-pro".to_string()];
    let test = build_resource_codex(&server, definition).await?;

    submit_prompt(&test.codex, "first reset attempt").await?;
    assert!(
        wait_for_error(&test.codex)
            .await
            .to_lowercase()
            .contains("usage limit")
    );
    wait_for_turn_complete(&test.codex).await;
    assert_eq!(resets.load(Ordering::SeqCst), 1);

    submit_prompt(&test.codex, "retry after the reset endpoint recovers").await?;
    if outcome == RetryOutcome::StillUnavailable {
        assert!(
            wait_for_error(&test.codex)
                .await
                .to_lowercase()
                .contains("usage limit")
        );
    }
    wait_for_turn_complete(&test.codex).await;
    if outcome == RetryOutcome::StillUnavailable {
        submit_prompt(&test.codex, "do not exceed the reset retry budget").await?;
        assert!(
            wait_for_error(&test.codex)
                .await
                .contains("no permitted pool capacity")
        );
        wait_for_turn_complete(&test.codex).await;
    }
    let reset_bodies = server
        .received_requests()
        .await
        .expect("request recording is enabled")
        .into_iter()
        .filter(|request| request.url.path() == "/api/codex/rate-limit-reset-credits/consume")
        .map(|request| {
            request
                .body_json::<serde_json::Value>()
                .expect("reset request body is JSON")
        })
        .collect::<Vec<_>>();
    assert_eq!(reset_bodies.len(), 2);
    assert_eq!(reset_bodies[0], reset_bodies[1]);
    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_reset_does_not_displace_the_next_recovery_candidate() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let restored = Arc::new(AtomicBool::new(false));
    let usage_restored = Arc::clone(&restored);
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(move |request: &wiremock::Request| {
            let allowed = request.headers["ChatGPT-Account-ID"] == "personal-pro"
                && usage_restored.load(Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "rate_limit": {"allowed": allowed},
                "rate_limit_reset_credits": {"available_count": 2}
            }))
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("ChatGPT-Account-ID", "work-pro"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let reset_restored = Arc::clone(&restored);
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("ChatGPT-Account-ID", "personal-pro"))
        .respond_with(move |_: &wiremock::Request| {
            reset_restored.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({"code": "reset"}))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |_: &wiremock::Request| {
            if restored.load(Ordering::SeqCst) {
                responses::sse_response(sse(vec![
                    ev_response_created("done"),
                    ev_completed("done"),
                ]))
            } else {
                usage_limit_response("codex", /*window_minutes*/ 300)
            }
        })
        .expect(3)
        .mount(&server)
        .await;
    let test = build_resource_codex(&server, resource_definition()).await?;
    test.submit_turn("recover using the next member").await?;
    let accounts = responses::received_responses_requests(&server)
        .await
        .iter()
        .map(|request| request.header("ChatGPT-Account-ID"))
        .collect::<Vec<_>>();
    assert_eq!(
        accounts,
        ["work-pro", "personal-pro", "personal-pro"].map(|account| Some(account.to_string()))
    );
    server.verify().await;
    Ok(())
}
