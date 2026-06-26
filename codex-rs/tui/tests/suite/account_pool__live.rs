use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use codex_app_server_protocol::AuthMode;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_pool_status_renders_in_live_tui() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;

    write_config(codex_home.path(), workspace.path(), &server.uri())?;
    write_chatgpt_auth(
        codex_home.path(),
        "work-pro",
        "work@example.com",
        "access-work",
        "pro",
    )?;
    write_chatgpt_auth(
        codex_home.path(),
        "personal-pro",
        "personal@example.com",
        "access-personal",
        "plus",
    )?;
    mount_usage_response(&server).await;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 100, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.4 default")
    })
    .await?;

    send_text(&writer, "/status ").await?;
    wait_for_screen(&mut output_rx, &mut screen, "status command", |contents| {
        contents.contains("/status")
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;

    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "account-pool status output",
        |contents| {
            contents.contains("codex-pro pool (2 members, 0 unavailable)")
                && contents.contains("active: work-pro / work@example.com / Pro")
        },
    )
    .await?;

    spawned.session.terminate();
    Ok(())
}

async fn mount_usage_response(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 42,
                    "limit_window_seconds": 3600,
                    "reset_after_seconds": 120,
                    "reset_at": 1_735_689_720,
                }
            }
        })))
        .mount(server)
        .await;
}

async fn spawn_tui(
    codex: &Path,
    codex_home: &Path,
    log_dir: &Path,
    workspace: &Path,
) -> Result<codex_utils_pty::SpawnedPty> {
    let env = HashMap::from([
        ("CODEX_HOME".to_string(), codex_home.display().to_string()),
        ("HOME".to_string(), codex_home.display().to_string()),
        ("RUST_LOG".to_string(), "trace".to_string()),
        ("TERM".to_string(), "xterm-256color".to_string()),
        (
            "CODEX_TUI_DISABLE_KEYBOARD_ENHANCEMENT".to_string(),
            "1".to_string(),
        ),
    ]);
    let args = vec![
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
        "-c".to_string(),
        format!("log_dir=\"{}\"", log_dir.display()),
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
    ];
    spawn_pty_process(
        &codex.display().to_string(),
        &args,
        workspace,
        &env,
        &None,
        TerminalSize {
            rows: 24,
            cols: 100,
        },
    )
    .await
}

fn write_config(codex_home: &Path, workspace: &Path, server_uri: &str) -> Result<()> {
    let workspace_display = workspace.display();
    let parent_display = workspace
        .parent()
        .unwrap_or(workspace)
        .display()
        .to_string();
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model = "gpt-5.4"
model_provider = "mock_provider"
chatgpt_base_url = "{server_uri}"
suppress_unstable_features_warning = true
approval_policy = "never"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = true

[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro", "personal-pro"]
policy = "drain"

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
        ),
    )?;
    Ok(())
}

fn write_chatgpt_auth(
    codex_home: &Path,
    account_id: &str,
    email: &str,
    access_token: &str,
    plan_type: &str,
) -> Result<()> {
    let account_home = codex_home.join("accounts").join(account_id);
    std::fs::create_dir_all(&account_home)?;
    let id_token = fake_jwt(json!({
        "email": email,
        "exp": chrono::Utc::now().timestamp() + 3600,
        "https://api.openai.com/auth": {
            "chatgpt_plan_type": plan_type,
            "chatgpt_account_id": account_id,
            "user_id": format!("user-{account_id}")
        }
    }))?;
    let auth = json!({
        "auth_mode": AuthMode::Chatgpt,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": format!("refresh-{account_id}"),
            "account_id": account_id,
        },
        "last_refresh": chrono::Utc::now(),
    });
    std::fs::write(
        account_home.join("auth.json"),
        serde_json::to_string_pretty(&auth)?,
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

async fn send_text(writer: &mpsc::Sender<Vec<u8>>, text: &str) -> Result<()> {
    for byte in text.as_bytes() {
        writer.send(vec![*byte]).await?;
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    Ok(())
}

async fn wait_for_screen(
    output_rx: &mut broadcast::Receiver<Vec<u8>>,
    screen: &mut vt100::Parser,
    label: &str,
    predicate: impl Fn(&str) -> bool,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 30);
    let mut raw = Vec::new();

    loop {
        let contents = screen.screen().contents();
        if predicate(&contents) {
            return Ok(contents);
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "timed out waiting for {label}; screen:\n{}\nraw:\n{}",
                screen.screen().contents(),
                String::from_utf8_lossy(&raw)
            );
        }

        let chunk =
            match tokio::time::timeout(deadline.saturating_duration_since(now), output_rx.recv())
                .await
            {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(err)) => {
                    anyhow::bail!(
                        "failed waiting for {label} output: {err}; screen:\n{}\nraw:\n{}",
                        screen.screen().contents(),
                        String::from_utf8_lossy(&raw)
                    );
                }
                Err(_) => {
                    anyhow::bail!(
                        "timed out waiting for {label} output; screen:\n{}\nraw:\n{}",
                        screen.screen().contents(),
                        String::from_utf8_lossy(&raw)
                    );
                }
            };
        raw.extend_from_slice(&chunk);
        screen.process(&chunk);
    }
}
