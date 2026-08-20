use codex_login::CODEX_ACCESS_TOKEN_ENV_VAR;
use codex_login::CODEX_API_KEY_ENV_VAR;
use codex_login::OPENAI_API_KEY_ENV_VAR;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::json;

const TEST_CHATGPT_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";
const ACCOUNT_POOL_CONFIG: &str = r#"
model = "ignored-model"

[account_pool]
enabled = true
default_pool = "primary"

[account_pool.pools.primary]
provider = "openai"
policy = "drain"
accounts = ["member"]
"#;

fn account_auth(access_token: &str) -> serde_json::Value {
    json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": TEST_CHATGPT_ID_TOKEN,
            "access_token": access_token,
            "refresh_token": "pool-refresh-token",
            "account_id": "account-123"
        },
        "last_refresh": "2099-01-01T00:00:00Z"
    })
}

fn write_file_account_auth(
    test: &core_test_support::test_codex_exec::TestCodexExecBuilder,
    access_token: &str,
) -> anyhow::Result<()> {
    std::fs::write(test.home_path().join("config.toml"), ACCOUNT_POOL_CONFIG)?;
    let account_home = test.home_path().join("accounts").join("member");
    std::fs::create_dir_all(&account_home)?;
    std::fs::write(
        account_home.join("auth.json"),
        serde_json::to_string_pretty(&account_auth(access_token))?,
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_ignore_user_config_preserves_account_pool_auth() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    write_file_account_auth(&test, "pool-token")?;
    let response_mock = mount_sse_once(&server, sse(vec![ev_completed("request_0")])).await;

    test.cmd_with_server(&server)
        .env_remove(CODEX_API_KEY_ENV_VAR)
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(OPENAI_API_KEY_ENV_VAR)
        .arg("--ignore-user-config")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&repo_root)
        .arg("echo testing account pool auth")
        .assert()
        .success();
    let request = response_mock.single_request();
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer pool-token")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_ignore_user_config_prefers_codex_api_key_over_account_pool() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    write_file_account_auth(&test, "pool-token")?;
    let response_mock = mount_sse_once(&server, sse(vec![ev_completed("request_0")])).await;

    test.cmd_with_server(&server)
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(OPENAI_API_KEY_ENV_VAR)
        .arg("--ignore-user-config")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&repo_root)
        .arg("echo testing codex api key precedence")
        .assert()
        .success();
    let request = response_mock.single_request();
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer dummy")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_ignore_user_config_honors_keyring_store_selection() -> anyhow::Result<()> {
    let test = test_codex_exec();
    let server = start_mock_server().await;
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    write_file_account_auth(&test, "file-pool-token")?;
    std::fs::write(
        test.home_path().join("config.toml"),
        format!("cli_auth_credentials_store = \"keyring\"\n{ACCOUNT_POOL_CONFIG}"),
    )?;
    let response_mock = mount_sse_once(&server, sse(vec![ev_completed("request_0")])).await;

    test.cmd_with_server(&server)
        .env_remove(CODEX_API_KEY_ENV_VAR)
        .env_remove(CODEX_ACCESS_TOKEN_ENV_VAR)
        .env_remove(OPENAI_API_KEY_ENV_VAR)
        .arg("--ignore-user-config")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(&repo_root)
        .arg("echo testing keyring store selection")
        .assert()
        .success();
    let request = response_mock.single_request();
    assert_eq!(request.header("authorization"), None);

    Ok(())
}
