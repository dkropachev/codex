use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::AccountPoolMember;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::GetAccountResponse;
use codex_app_server_protocol::JSONRPCResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::login_with_api_key;
use codex_protocol::account::PlanType as AccountPlanType;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct CreateConfigTomlParams {
    requires_openai_auth: Option<bool>,
    account_pool_config: Option<String>,
}

fn create_config_toml(codex_home: &Path, params: CreateConfigTomlParams) -> std::io::Result<()> {
    let requires_line = match params.requires_openai_auth {
        Some(true) => "requires_openai_auth = true\n".to_string(),
        Some(false) | None => String::new(),
    };
    let account_pool_config = params.account_pool_config.unwrap_or_default();
    let contents = format!(
        r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"

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
            active_account_id: Some("work-pro".to_string()),
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: true,
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
            active_account_id: Some("work-pro".to_string()),
            members: vec![
                AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(AccountPlanType::Pro),
                    active: true,
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

async fn read_account_response(codex_home: &Path) -> Result<GetAccountResponse> {
    let mut mcp = TestAppServer::new_with_env(codex_home, &[("OPENAI_API_KEY", None)]).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

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
