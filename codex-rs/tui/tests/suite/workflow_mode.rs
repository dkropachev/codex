use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use core_test_support::responses;
use tempfile::tempdir;
use tokio::sync::broadcast;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_slash_enters_mode_and_submits_mocked_ai_turn() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;
    let response_mock = responses::mount_sse_once(&server, workflow_mode_sse()).await;

    write_config(codex_home.path(), workspace.path(), &server.uri())?;
    write_auth(codex_home.path())?;

    let env = HashMap::from([
        (
            "CODEX_HOME".to_string(),
            codex_home.path().display().to_string(),
        ),
        ("HOME".to_string(), codex_home.path().display().to_string()),
        ("OPENAI_API_KEY".to_string(), "dummy".to_string()),
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
        format!("log_dir=\"{}\"", log_dir.path().display()),
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.path().display().to_string(),
    ];
    let spawned = spawn_pty_process(
        &codex.display().to_string(),
        &args,
        workspace.path(),
        &env,
        &None,
        TerminalSize { rows: 24, cols: 80 },
    )
    .await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.4 default")
    })
    .await?;

    writer.send(b"/workflow".to_vec()).await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(&mut output_rx, &mut screen, "workflow footer", |contents| {
        contents.contains("Workflow mode")
    })
    .await?;

    writer
        .send(b"TUI workflow mode e2e prompt".to_vec())
        .await?;
    wait_for_screen(&mut output_rx, &mut screen, "prompt draft", |contents| {
        contents.contains("TUI workflow mode e2e prompt")
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "assistant response",
        |contents| contents.contains("workflow e2e sentinel"),
    )
    .await?;

    spawned.session.terminate();

    let request = response_mock.single_request();
    anyhow::ensure!(
        request.body_contains_text("TUI workflow mode e2e prompt"),
        "mocked Responses request did not include the submitted prompt: {}",
        request.body_json()
    );
    anyhow::ensure!(
        request.body_contains_text(
            "Workflow mode exists to design, inspect, tune, validate, repair, and explain Codex workflows."
        ),
        "mocked Responses request did not include Workflow-mode instructions: {}",
        request.body_json()
    );

    Ok(())
}

fn write_config(
    codex_home: &std::path::Path,
    workspace: &std::path::Path,
    server_uri: &str,
) -> Result<()> {
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
suppress_unstable_features_warning = true

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[features]
workflows = true

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
        ),
    )?;
    Ok(())
}

fn write_auth(codex_home: &std::path::Path) -> Result<()> {
    std::fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;
    Ok(())
}

fn workflow_mode_sse() -> String {
    responses::sse(vec![
        responses::ev_response_created("resp-workflow-mode-e2e"),
        responses::ev_assistant_message("msg-workflow-mode-e2e", "workflow e2e sentinel"),
        responses::ev_completed("resp-workflow-mode-e2e"),
    ])
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
                    return Err(err).with_context(|| format!("failed waiting for {label} output"));
                }
                Err(_) => {
                    anyhow::bail!(
                        "timed out waiting for {label} output; screen:\n{}\nraw:\n{}",
                        screen.screen().contents(),
                        String::from_utf8_lossy(&raw)
                    );
                }
            };
        screen.write_all(&chunk)?;
        raw.extend_from_slice(&chunk);
    }
}
