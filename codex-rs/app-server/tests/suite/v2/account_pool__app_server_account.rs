use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountPoolMember;
use codex_app_server_protocol::AccountTokenUsageDailyBucket;
use codex_app_server_protocol::AccountTokenUsageSummary;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::GetAccountTokenUsageResponse;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::login_with_api_key;
use codex_protocol::account::PlanType as AccountPlanType;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct CreateConfigTomlParams {
    requires_openai_auth: Option<bool>,
    account_pool_config: Option<String>,
    chatgpt_base_url: Option<String>,
}

fn create_config_toml(codex_home: &Path, params: CreateConfigTomlParams) -> std::io::Result<()> {
    let requires_line = match params.requires_openai_auth {
        Some(true) => "requires_openai_auth = true\n".to_string(),
        Some(false) | None => String::new(),
    };
    let chatgpt_base_url_line = params
        .chatgpt_base_url
        .map(|base_url| format!("chatgpt_base_url = \"{base_url}\"\n"))
        .unwrap_or_default();
    let account_pool_config = params.account_pool_config.unwrap_or_default();
    let contents = format!(
        r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"
{chatgpt_base_url_line}

model_provider = "mock_provider"

[features]
shell_snapshot = false

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "http://127.0.0.1:0/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
{requires_line}

{account_pool_config}
"#
    );
    fs::write(codex_home.join("config.toml"), contents)
}

#[tokio::test]
async fn get_account_with_chatgpt_pool() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            chatgpt_base_url: None,
            account_pool_config: Some(
                r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
policy = "drain"
accounts = ["work-pro", "personal-pro"]
"#
                .to_string(),
            ),
        },
    )?;

    let work_home = codex_home.path().join("accounts/work-pro");
    fs::create_dir_all(&work_home)?;
    write_chatgpt_auth(
        &work_home,
        ChatGptAuthFixture::new("access-work")
            .account_id("work-pro")
            .chatgpt_account_id("work-pro")
            .email("work@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let personal_home = codex_home.path().join("accounts/personal-pro");
    fs::create_dir_all(&personal_home)?;
    write_chatgpt_auth(
        &personal_home,
        ChatGptAuthFixture::new("access-personal")
            .account_id("personal-pro")
            .chatgpt_account_id("personal-pro")
            .email("personal@example.com")
            .plan_type("plus"),
        AuthCredentialsStoreMode::File,
    )?;

    let received = read_account_response(codex_home.path()).await?;

    let expected = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: None,
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMember {
                    id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Plus),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
            ],
        }),
        requires_openai_auth: true,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn get_account_with_chatgpt_pool_reports_unavailable_members() -> Result<()> {
    let codex_home = TempDir::new()?;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            chatgpt_base_url: None,
            account_pool_config: Some(
                r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
policy = "drain"
accounts = ["work-pro", "api-key-pro", "missing-pro"]
"#
                .to_string(),
            ),
        },
    )?;

    let work_home = codex_home.path().join("accounts/work-pro");
    fs::create_dir_all(&work_home)?;
    write_chatgpt_auth(
        &work_home,
        ChatGptAuthFixture::new("access-work")
            .account_id("work-pro")
            .chatgpt_account_id("work-pro")
            .email("work@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let api_key_home = codex_home.path().join("accounts/api-key-pro");
    fs::create_dir_all(&api_key_home)?;
    login_with_api_key(&api_key_home, "sk-test-key", AuthCredentialsStoreMode::File)?;

    let received = read_account_response(codex_home.path()).await?;

    let expected = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: None,
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMember {
                    id: "api-key-pro".to_string(),
                    email: None,
                    plan_type: None,
                    active: false,
                    unavailable_reason: Some(
                        "account pool members must use ChatGPT auth".to_string(),
                    ),
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMember {
                    id: "missing-pro".to_string(),
                    email: None,
                    plan_type: None,
                    active: false,
                    unavailable_reason: Some("missing credentials".to_string()),
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
            ],
        }),
        requires_openai_auth: true,
    };
    assert_eq!(received, expected);
    Ok(())
}

#[tokio::test]
async fn get_account_rate_limits_read_does_not_activate_chatgpt_pool_member() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            chatgpt_base_url: Some(server.uri()),
            account_pool_config: Some(
                r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
policy = "drain"
accounts = ["work-pro", "personal-pro"]
"#
                .to_string(),
            ),
        },
    )?;

    let work_home = codex_home.path().join("accounts/work-pro");
    fs::create_dir_all(&work_home)?;
    write_chatgpt_auth(
        &work_home,
        ChatGptAuthFixture::new("access-work")
            .account_id("work-pro")
            .chatgpt_account_id("work-pro")
            .email("work@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let personal_home = codex_home.path().join("accounts/personal-pro");
    fs::create_dir_all(&personal_home)?;
    write_chatgpt_auth(
        &personal_home,
        ChatGptAuthFixture::new("access-personal")
            .account_id("personal-pro")
            .chatgpt_account_id("personal-pro")
            .email("personal@example.com")
            .plan_type("plus"),
        AuthCredentialsStoreMode::File,
    )?;

    let reset_timestamp = chrono::DateTime::parse_from_rfc3339("2025-01-01T00:02:00Z")
        .expect("parse reset timestamp")
        .timestamp();
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header("authorization", "Bearer access-work"))
        .and(header("chatgpt-account-id", "work-pro"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 42,
                    "limit_window_seconds": 3600,
                    "reset_after_seconds": 120,
                    "reset_at": reset_timestamp,
                }
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp =
        TestAppServer::new_with_env(codex_home.path(), &[("OPENAI_API_KEY", None)]).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let expected_account = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: None,
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMember {
                    id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Plus),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
            ],
        }),
        requires_openai_auth: true,
    };
    assert_eq!(
        read_account_response_from(&mut mcp).await?,
        expected_account
    );

    let rate_limits = read_rate_limits_response_from(&mut mcp).await?;
    assert_eq!(
        rate_limits,
        GetAccountRateLimitsResponse {
            rate_limits: RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 42,
                    window_duration_mins: Some(60),
                    resets_at: Some(reset_timestamp),
                }),
                secondary: None,
                credits: None,
                individual_limit: None,
                plan_type: Some(AccountPlanType::Pro),
                rate_limit_reached_type: None,
            },
            rate_limits_by_limit_id: Some(
                [(
                    "codex".to_string(),
                    RateLimitSnapshot {
                        limit_id: Some("codex".to_string()),
                        limit_name: None,
                        primary: Some(RateLimitWindow {
                            used_percent: 42,
                            window_duration_mins: Some(60),
                            resets_at: Some(reset_timestamp),
                        }),
                        secondary: None,
                        credits: None,
                        individual_limit: None,
                        plan_type: Some(AccountPlanType::Pro),
                        rate_limit_reached_type: None,
                    },
                )]
                .into_iter()
                .collect(),
            ),
        }
    );

    assert_eq!(
        read_account_response_from(&mut mcp).await?,
        expected_account
    );
    Ok(())
}

#[tokio::test]
async fn get_account_token_usage_read_does_not_activate_chatgpt_pool_member() -> Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    create_config_toml(
        codex_home.path(),
        CreateConfigTomlParams {
            requires_openai_auth: Some(true),
            chatgpt_base_url: Some(server.uri()),
            account_pool_config: Some(
                r#"
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
policy = "drain"
accounts = ["work-pro", "personal-pro"]
"#
                .to_string(),
            ),
        },
    )?;

    let work_home = codex_home.path().join("accounts/work-pro");
    fs::create_dir_all(&work_home)?;
    write_chatgpt_auth(
        &work_home,
        ChatGptAuthFixture::new("access-work")
            .account_id("work-pro")
            .chatgpt_account_id("work-pro")
            .email("work@example.com")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let personal_home = codex_home.path().join("accounts/personal-pro");
    fs::create_dir_all(&personal_home)?;
    write_chatgpt_auth(
        &personal_home,
        ChatGptAuthFixture::new("access-personal")
            .account_id("personal-pro")
            .chatgpt_account_id("personal-pro")
            .email("personal@example.com")
            .plan_type("plus"),
        AuthCredentialsStoreMode::File,
    )?;

    Mock::given(method("GET"))
        .and(path("/api/codex/profiles/me"))
        .and(header("authorization", "Bearer access-work"))
        .and(header("chatgpt-account-id", "work-pro"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "stats": {
                "lifetime_tokens": 123,
                "peak_daily_tokens": 45,
                "longest_running_turn_sec": 67,
                "current_streak_days": 8,
                "longest_streak_days": 9,
                "daily_usage_buckets": [
                    { "start_date": "2026-05-29", "tokens": 10 }
                ]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp =
        TestAppServer::new_with_env(codex_home.path(), &[("OPENAI_API_KEY", None)]).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let expected_account = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: None,
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
                AccountPoolMember {
                    id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Plus),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: None,
                },
            ],
        }),
        requires_openai_auth: true,
    };
    assert_eq!(
        read_account_response_from(&mut mcp).await?,
        expected_account
    );

    let token_usage = read_token_usage_response_from(&mut mcp).await?;
    assert_eq!(
        token_usage,
        GetAccountTokenUsageResponse {
            summary: AccountTokenUsageSummary {
                lifetime_tokens: Some(123),
                peak_daily_tokens: Some(45),
                longest_running_turn_sec: Some(67),
                current_streak_days: Some(8),
                longest_streak_days: Some(9),
            },
            daily_usage_buckets: Some(vec![AccountTokenUsageDailyBucket {
                start_date: "2026-05-29".to_string(),
                tokens: 10,
            }]),
        }
    );

    assert_eq!(
        read_account_response_from(&mut mcp).await?,
        expected_account
    );
    Ok(())
}

async fn read_account_response(codex_home: &Path) -> Result<GetAccountResponse> {
    let mut mcp = TestAppServer::new_with_env(codex_home, &[("OPENAI_API_KEY", None)]).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    read_account_response_from(&mut mcp).await
}

async fn read_account_response_from(mcp: &mut TestAppServer) -> Result<GetAccountResponse> {
    let request_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(resp)
}

async fn read_rate_limits_response_from(
    mcp: &mut TestAppServer,
) -> Result<GetAccountRateLimitsResponse> {
    let request_id = mcp.send_get_account_rate_limits_request().await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(resp)
}

async fn read_token_usage_response_from(
    mcp: &mut TestAppServer,
) -> Result<GetAccountTokenUsageResponse> {
    let request_id = mcp.send_get_account_token_usage_request().await?;
    let resp: JSONRPCResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    to_response(resp)
}
