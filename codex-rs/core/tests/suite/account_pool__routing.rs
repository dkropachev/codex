use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use base64::Engine as _;
use chrono::Utc;
use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::WebSocketConnectionConfig;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_websocket_server_with_headers;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_retries_regular_usage_limit_with_next_member() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    mount_usage_limit_response(&server, "work-pro", "codex", /*window_minutes*/ 300).await;
    mount_success_response(&server, "personal-pro").await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "use regular pool").await?;
    wait_for_turn_complete(&codex).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_retries_spark_usage_limit_with_next_member() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    mount_usage_limit_response(
        &server,
        "work-pro",
        "bengalfox",
        /*window_minutes*/ 300,
    )
    .await;
    mount_success_response(&server, "personal-pro").await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "use spark pool").await?;
    wait_for_turn_complete(&codex).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_retries_short_usage_limit_when_wait_exceeds_cache_cost() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    mount_usage_limit_response(&server, "work-pro", "codex", /*window_minutes*/ 15).await;
    mount_success_response(&server, "personal-pro").await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "short usage limit").await?;
    wait_for_turn_complete(&codex).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_keeps_hot_cache_for_short_wait_usage_limit() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;
    let work_requests = Arc::new(AtomicUsize::new(0));
    let personal_requests = Arc::new(AtomicUsize::new(0));
    let responder_work_requests = Arc::clone(&work_requests);
    let responder_personal_requests = Arc::clone(&personal_requests);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |request: &wiremock::Request| {
            match request
                .headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok())
            {
                Some("work-pro") => {
                    let count = responder_work_requests.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        ResponseTemplate::new(200)
                            .insert_header("content-type", "text/event-stream")
                            .set_body_string(sse(vec![
                                ev_response_created("resp-hot"),
                                ev_assistant_message("msg-hot", "warm cache"),
                                ev_completed_with_cached_tokens("resp-hot"),
                            ]))
                    } else {
                        usage_limit_response_with_reset(
                            "codex",
                            /*window_minutes*/ 15,
                            /*reset_after_seconds*/ 5 * 60,
                        )
                    }
                }
                Some("personal-pro") => {
                    responder_personal_requests.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(200)
                        .insert_header("content-type", "text/event-stream")
                        .set_body_string(sse(vec![
                            ev_response_created("resp-unexpected"),
                            ev_assistant_message("msg-unexpected", "unexpected"),
                            ev_completed("resp-unexpected"),
                        ]))
                }
                _ => ResponseTemplate::new(400),
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "create hot cache").await?;
    wait_for_turn_complete(&codex).await;

    submit_prompt(&codex, "short wait limit").await?;
    let error = wait_for_error(&codex).await;
    wait_for_turn_complete(&codex).await;

    assert!(error.to_lowercase().contains("usage limit"));
    assert_eq!(work_requests.load(Ordering::SeqCst), 2);
    assert_eq!(personal_requests.load(Ordering::SeqCst), 0);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_websocket_failover_reconnects_with_next_member() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let usage_limit_error = json!({
        "type": "error",
        "status": 429,
        "error": {
            "type": "usage_limit_reached",
            "message": "usage limit reached",
            "plan_type": "pro",
            "resets_at": Utc::now().timestamp() + 60 * 60,
            "resets_in_seconds": 60 * 60
        },
        "headers": {
            "x-codex-active-limit": "codex",
            "x-codex-primary-used-percent": "100.0",
            "x-codex-primary-window-minutes": "300"
        }
    });

    let server = start_websocket_server_with_headers(vec![
        WebSocketConnectionConfig {
            requests: vec![
                vec![
                    ev_response_created("resp-prewarm"),
                    ev_completed("resp-prewarm"),
                ],
                vec![
                    ev_response_created("resp-work"),
                    ev_assistant_message("msg-work", "work account response"),
                    ev_completed("resp-work"),
                ],
                vec![usage_limit_error],
            ],
            response_headers: Vec::new(),
            accept_delay: None,
            close_after_requests: false,
        },
        WebSocketConnectionConfig {
            requests: vec![vec![
                ev_response_created("resp-personal"),
                ev_assistant_message("msg-personal", "personal account response"),
                ev_completed("resp-personal"),
            ]],
            response_headers: Vec::new(),
            accept_delay: None,
            close_after_requests: true,
        },
    ])
    .await;

    let codex = build_account_pool_websocket_codex(&server).await?.codex;
    submit_prompt(&codex, "first websocket turn").await?;
    wait_for_turn_complete(&codex).await;

    submit_prompt(&codex, "second websocket turn").await?;
    wait_for_turn_complete(&codex).await;

    assert!(
        server
            .wait_for_handshakes(/*expected*/ 2, Duration::from_secs(5))
            .await
    );

    let handshakes = server.handshakes();
    assert_eq!(handshakes.len(), 2);
    assert_eq!(
        handshakes[0].header("ChatGPT-Account-ID").as_deref(),
        Some("work-pro")
    );
    assert_eq!(
        handshakes[1].header("ChatGPT-Account-ID").as_deref(),
        Some("personal-pro")
    );

    let connections = server.connections();
    assert_eq!(connections.len(), 2);
    assert_eq!(connections[0].len(), 3);
    assert_eq!(connections[1].len(), 1);

    let work_retry = connections[0][2].body_json();
    assert_eq!(work_retry["type"].as_str(), Some("response.create"));
    assert_eq!(
        work_retry["previous_response_id"].as_str(),
        Some("resp-work")
    );

    let personal_retry = connections[1][0].body_json();
    assert_eq!(personal_retry["type"].as_str(), Some("response.create"));
    assert_eq!(personal_retry.get("previous_response_id"), None);
    assert!(personal_retry.to_string().contains("second websocket turn"));

    server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_does_not_retry_usage_not_included_with_next_member() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let requests = mount_sse_sequence(
        &server,
        vec![responses::sse_failed(
            "resp-usage-not-included",
            "usage_not_included",
            "Usage is not included for this account.",
        )],
    )
    .await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "use included account").await?;
    let error = wait_for_error(&codex).await;
    wait_for_turn_complete(&codex).await;

    assert!(error.to_lowercase().contains("upgrade to plus"));
    let requests = requests.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("ChatGPT-Account-ID").as_deref(),
        Some("work-pro")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_does_not_retry_usage_error_after_visible_output() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let requests = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-visible"),
            ev_assistant_message("msg-visible", "partial"),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp-visible",
                    "error": {
                        "code": "usage_not_included",
                        "message": "Usage is not included for this account."
                    }
                }
            }),
        ]),
    )
    .await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "visible then usage error").await?;
    let error = wait_for_error(&codex).await;
    wait_for_turn_complete(&codex).await;

    assert!(error.to_lowercase().contains("upgrade to plus"));
    let requests = requests.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("ChatGPT-Account-ID").as_deref(),
        Some("work-pro")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usage_limit_without_account_pool_surfaces_original_error() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(usage_limit_response("codex", /*window_minutes*/ 15))
        .expect(1)
        .mount(&server)
        .await;

    let mut builder = test_codex();
    let codex = builder.build(&server).await?.codex;
    submit_prompt(&codex, "no pool").await?;
    let error = wait_for_error(&codex).await;
    wait_for_turn_complete(&codex).await;

    assert!(error.to_lowercase().contains("usage limit"));

    Ok(())
}

async fn build_account_pool_codex(
    server: &MockServer,
) -> Result<core_test_support::test_codex::TestCodex> {
    let mut builder = test_codex()
        .with_config_auth_manager()
        .with_pre_build_hook(|codex_home| {
            write_chatgpt_auth(codex_home, "work-pro", "work@example.com");
            write_chatgpt_auth(codex_home, "personal-pro", "personal@example.com");
        })
        .with_config(|config| {
            config.account_pool = Some(AccountPoolToml {
                enabled: true,
                default_pool: Some("codex-pro".to_string()),
                pools: [(
                    "codex-pro".to_string(),
                    AccountPoolDefinitionToml {
                        provider: "openai".to_string(),
                        policy: AccountPoolPolicyToml::Drain,
                        accounts: vec!["work-pro".to_string(), "personal-pro".to_string()],
                    },
                )]
                .into(),
            });
        });
    builder.build(server).await
}

async fn build_account_pool_websocket_codex(
    server: &responses::WebSocketTestServer,
) -> Result<core_test_support::test_codex::TestCodex> {
    let mut builder = test_codex()
        .with_config_auth_manager()
        .with_pre_build_hook(|codex_home| {
            write_chatgpt_auth(codex_home, "work-pro", "work@example.com");
            write_chatgpt_auth(codex_home, "personal-pro", "personal@example.com");
        })
        .with_config(|config| {
            config.account_pool = Some(AccountPoolToml {
                enabled: true,
                default_pool: Some("codex-pro".to_string()),
                pools: [(
                    "codex-pro".to_string(),
                    AccountPoolDefinitionToml {
                        provider: "openai".to_string(),
                        policy: AccountPoolPolicyToml::Drain,
                        accounts: vec!["work-pro".to_string(), "personal-pro".to_string()],
                    },
                )]
                .into(),
            });
        });
    builder.build_with_websocket_server(server).await
}

async fn mount_usage_limit_response(
    server: &MockServer,
    account_id: &str,
    limit_id: &str,
    window_minutes: i64,
) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("ChatGPT-Account-ID", account_id))
        .respond_with(usage_limit_response(limit_id, window_minutes))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

async fn mount_success_response(server: &MockServer, account_id: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("ChatGPT-Account-ID", account_id))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(vec![
                    ev_response_created("resp-ok"),
                    ev_assistant_message("msg-ok", "done"),
                    ev_completed("resp-ok"),
                ])),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

fn usage_limit_response(limit_id: &str, window_minutes: i64) -> ResponseTemplate {
    usage_limit_response_with_reset(
        limit_id,
        window_minutes,
        /*reset_after_seconds*/ 60 * 60,
    )
}

fn usage_limit_response_with_reset(
    limit_id: &str,
    window_minutes: i64,
    reset_after_seconds: i64,
) -> ResponseTemplate {
    let mut response = ResponseTemplate::new(429)
        .insert_header("x-codex-active-limit", limit_id)
        .insert_header("x-codex-primary-used-percent", "100.0")
        .insert_header("x-codex-primary-window-minutes", window_minutes.to_string())
        .set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "usage limit reached",
                "resets_at": Utc::now().timestamp() + reset_after_seconds,
                "plan_type": "pro"
            }
        }));
    if limit_id != "codex" {
        response = response
            .insert_header(format!("x-{limit_id}-primary-used-percent"), "100.0")
            .insert_header(
                format!("x-{limit_id}-primary-window-minutes"),
                window_minutes.to_string(),
            );
    }
    response
}

fn ev_completed_with_cached_tokens(id: &str) -> serde_json::Value {
    json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "usage": {
                "input_tokens": 20_000,
                "input_tokens_details": {
                    "cached_tokens": 12_000
                },
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": 20_000
            }
        }
    })
}

async fn submit_prompt(codex: &codex_core::CodexThread, text: &str) -> Result<()> {
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    Ok(())
}

async fn wait_for_turn_complete(codex: &codex_core::CodexThread) {
    wait_for_event(codex, |event| matches!(event, EventMsg::TurnComplete(_))).await;
}

async fn wait_for_error(codex: &codex_core::CodexThread) -> String {
    let EventMsg::Error(error) =
        wait_for_event(codex, |event| matches!(event, EventMsg::Error(_))).await
    else {
        unreachable!();
    };
    error.message
}

#[expect(clippy::expect_used)]
fn write_chatgpt_auth(codex_home: &Path, account_id: &str, email: &str) {
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

#[expect(clippy::expect_used)]
fn fake_jwt(payload: serde_json::Value) -> String {
    let header = json!({"alg": "none"});
    let encode = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&value).expect("serialize jwt part"))
    };
    format!("{}.{}.sig", encode(header), encode(payload))
}
