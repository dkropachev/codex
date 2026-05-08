use std::fs;
use std::path::Path;

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
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
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
    mount_usage_limit_response(&server, "work-pro", "codex").await;
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
    mount_usage_limit_response(&server, "work-pro", "bengalfox").await;
    mount_success_response(&server, "personal-pro").await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "use spark pool").await?;
    wait_for_turn_complete(&codex).await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_retries_usage_not_included_with_next_regular_member() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let requests = mount_sse_sequence(
        &server,
        vec![
            responses::sse_failed(
                "resp-usage-not-included",
                "usage_not_included",
                "Usage is not included for this account.",
            ),
            sse(vec![
                ev_response_created("resp-ok"),
                ev_assistant_message("msg-ok", "done"),
                ev_completed("resp-ok"),
            ]),
        ],
    )
    .await;

    let codex = build_account_pool_codex(&server).await?.codex;
    submit_prompt(&codex, "use included account").await?;
    wait_for_turn_complete(&codex).await;

    let requests = requests.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("ChatGPT-Account-ID").as_deref(),
        Some("work-pro")
    );
    assert_eq!(
        requests[1].header("ChatGPT-Account-ID").as_deref(),
        Some("personal-pro")
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
        .respond_with(usage_limit_response("codex"))
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

async fn mount_usage_limit_response(server: &MockServer, account_id: &str, limit_id: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("ChatGPT-Account-ID", account_id))
        .respond_with(usage_limit_response(limit_id))
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

fn usage_limit_response(limit_id: &str) -> ResponseTemplate {
    let mut response = ResponseTemplate::new(429)
        .insert_header("x-codex-active-limit", limit_id)
        .insert_header("x-codex-primary-used-percent", "100.0")
        .insert_header("x-codex-primary-window-minutes", "15")
        .set_body_json(json!({
            "error": {
                "type": "usage_limit_reached",
                "message": "usage limit reached",
                "resets_at": Utc::now().timestamp() + 3600,
                "plan_type": "pro"
            }
        }));
    if limit_id != "codex" {
        response = response
            .insert_header(format!("x-{limit_id}-primary-used-percent"), "100.0")
            .insert_header(format!("x-{limit_id}-primary-window-minutes"), "15");
    }
    response
}

async fn submit_prompt(codex: &codex_core::CodexThread, text: &str) -> Result<()> {
    codex
        .submit(Op::UserInput {
            environments: None,
            items: vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
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
