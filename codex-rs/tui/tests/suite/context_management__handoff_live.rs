use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
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

const SOURCE_PROMPT: &str = "source-only handoff context marker";
const SOURCE_RESPONSE: &str = "source turn complete";
const HANDOFF_PLAN: &str = "# Continue safely\n\n1. Execute the focused handoff test plan.\n2. Verify the fresh session result.";
const EXECUTION_RESPONSE: &str = "fresh handoff execution sentinel";
const SOURCE_THREAD_NAME: &str = "src";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn automatic_handoff_wraps_up_then_plans_and_transfers() -> Result<()> {
    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;

    write_config(codex_home.path(), workspace.path(), &server.uri())?;
    let mut config = std::fs::OpenOptions::new()
        .append(true)
        .open(codex_home.path().join("config.toml"))?;
    writeln!(config, "\n[tui]\nauto_handoff_threshold_percent = 80")?;
    write_auth(codex_home.path())?;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 60, /*cols*/ 160, /*scrollback*/ 0);
    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("Ask Codex to do anything")
    })
    .await?;
    rename_source_thread(&writer, &mut output_rx, &mut screen).await?;

    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            source_turn_with_high_usage_sse(),
            responses::sse(vec![
                responses::ev_response_created("resp-auto-wrap-up"),
                responses::ev_assistant_message("msg-auto-wrap-up", "Atomic work is complete."),
                responses::ev_completed("resp-auto-wrap-up"),
            ]),
            handoff_plan_sse(),
            execution_turn_sse(),
            thread_title_sse("automatic"),
        ],
    )
    .await;

    let result = async {
        writer.send(SOURCE_PROMPT.as_bytes().to_vec()).await?;
        wait_for_screen(
            &mut output_rx,
            &mut screen,
            "source prompt draft",
            |contents| contents.contains(SOURCE_PROMPT),
        )
        .await?;
        writer.send(b"\r".to_vec()).await?;
        wait_for_screen(
            &mut output_rx,
            &mut screen,
            "automatic execution",
            |contents| contents.contains(EXECUTION_RESPONSE),
        )
        .await?;

        let requests = wait_for_request_count(&response_mock, /*expected*/ 5).await?;
        anyhow::ensure!(
            requests.len() == 5,
            "expected five automatic handoff requests"
        );
        let source_thread_id = requests[0].body_json()["client_metadata"]["thread_id"]
            .as_str()
            .context("source request missing thread ID")?
            .to_string();
        for request in &requests[1..3] {
            anyhow::ensure!(
                request.body_json()["client_metadata"]["thread_id"].as_str()
                    == Some(source_thread_id.as_str()),
                "automatic wrap-up or planning left the source thread"
            );
        }
        anyhow::ensure!(
            requests[3].body_json()["client_metadata"]["thread_id"].as_str()
                != Some(source_thread_id.as_str()),
            "automatic execution reused the source thread"
        );
        anyhow::ensure!(
            requests[1].body_contains_text("Prepare this task for an automatic session handoff"),
            "automatic wrap-up prompt was not submitted"
        );
        anyhow::ensure!(
            requests[2].body_contains_text("Prepare a focused handoff plan"),
            "automatic planning prompt was not submitted"
        );
        anyhow::ensure!(
            requests[3].body_contains_text(HANDOFF_PLAN)
                && !requests[3].body_contains_text(SOURCE_PROMPT),
            "fresh execution did not isolate the handoff plan"
        );
        Ok(())
    }
    .await;
    spawned.session.terminate();
    if result.is_err() {
        server.reset().await;
    }
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_handoff_default_transfers_to_fresh_thread_and_source_remains_resumable() -> Result<()>
{
    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;

    write_config(codex_home.path(), workspace.path(), &server.uri())?;
    write_auth(codex_home.path())?;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 60, /*cols*/ 160, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("Ask Codex to do anything")
    })
    .await?;
    rename_source_thread(&writer, &mut output_rx, &mut screen).await?;
    let response_mock = responses::mount_sse_sequence(
        &server,
        vec![
            source_turn_sse(),
            handoff_plan_sse(),
            execution_turn_sse(),
            thread_title_sse("manual"),
        ],
    )
    .await;

    let result = async {
        writer.send(SOURCE_PROMPT.as_bytes().to_vec()).await?;
        wait_for_screen(
            &mut output_rx,
            &mut screen,
            "source prompt draft",
            |contents| contents.contains(SOURCE_PROMPT),
        )
        .await?;
        writer.send(b"\r".to_vec()).await?;
        wait_for_screen(&mut output_rx, &mut screen, "source response", |contents| {
            contents.contains(SOURCE_RESPONSE)
        })
        .await?;
        let busy_notice = "'/handoff' is disabled while a task is in progress.";
        let handoff_deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 10);
        loop {
            let prior_busy_notices = screen.screen().contents().matches(busy_notice).count();
            writer.send(b"/handoff".to_vec()).await?;
            wait_for_screen(
                &mut output_rx,
                &mut screen,
                "handoff command draft",
                |contents| contents.contains("› /handoff"),
            )
            .await?;
            writer.send(b"\r".to_vec()).await?;
            let contents = wait_for_screen(
                &mut output_rx,
                &mut screen,
                "handoff execution or busy notice",
                |contents| {
                    contents.contains(EXECUTION_RESPONSE)
                        || contents.matches(busy_notice).count() > prior_busy_notices
                },
            )
            .await?;
            if contents.contains(EXECUTION_RESPONSE) {
                break;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < handoff_deadline,
                "handoff remained busy after source completion; screen:\n{}",
                screen.screen().contents()
            );
            tokio::time::sleep(Duration::from_millis(/*millis*/ 250)).await;
        }

        let requests = wait_for_request_count(&response_mock, /*expected*/ 4).await?;
        anyhow::ensure!(
            requests.len() == 4,
            "expected source, planning, execution, and title requests, got {}",
            requests.len()
        );
        let source_body = requests[0].body_json();
        let planning_body = requests[1].body_json();
        let execution_body = requests[2].body_json();
        let source_thread_id = source_body["client_metadata"]["thread_id"]
            .as_str()
            .context("source request was missing client_metadata.thread_id")?;
        let planning_thread_id = planning_body["client_metadata"]["thread_id"]
            .as_str()
            .context("planning request was missing client_metadata.thread_id")?;
        let execution_thread_id = execution_body["client_metadata"]["thread_id"]
            .as_str()
            .context("execution request was missing client_metadata.thread_id")?;
        anyhow::ensure!(
            source_thread_id == planning_thread_id,
            "manual handoff planning did not run in the source thread"
        );
        anyhow::ensure!(
            source_thread_id != execution_thread_id,
            "manual handoff execution reused the source thread"
        );
        anyhow::ensure!(
            requests[0].body_contains_text(SOURCE_PROMPT),
            "source request did not include the original prompt"
        );
        anyhow::ensure!(
            requests[1].body_contains_text("Prepare a safe handoff plan"),
            "planning request did not include handoff instructions: {:?}",
            requests[1].message_input_texts("user")
        );
        anyhow::ensure!(
            requests[2].body_contains_text(HANDOFF_PLAN)
                && requests[2].body_contains_text(
                    "A previous session prepared the authoritative handoff plan below"
                ),
            "fresh execution request did not include the bounded handoff payload"
        );
        for source_only_text in [
            SOURCE_PROMPT,
            SOURCE_RESPONSE,
            "Prepare a safe handoff plan",
        ] {
            anyhow::ensure!(
                !requests[2].body_contains_text(source_only_text),
                "fresh execution request leaked source-only text `{source_only_text}`"
            );
        }

        let resume_command = format!("/resume {source_thread_id}");
        writer.send(resume_command.as_bytes().to_vec()).await?;
        wait_for_screen(
            &mut output_rx,
            &mut screen,
            "resume command draft",
            |contents| contents.contains(&resume_command),
        )
        .await?;
        writer.send(b"\r".to_vec()).await?;
        wait_for_screen(
            &mut output_rx,
            &mut screen,
            "resumed source thread",
            |contents| contents.contains(SOURCE_PROMPT) && contents.contains(SOURCE_RESPONSE),
        )
        .await?;

        anyhow::ensure!(
            response_mock.requests().len() == 4,
            "resuming the source unexpectedly submitted another model request"
        );
        Ok(())
    }
    .await;
    spawned.session.terminate();
    if result.is_err() {
        server.reset().await;
    }
    result
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
            r#"model = "gpt-5.6-terra"
model_provider = "mock_provider"
model_context_window = 100000
suppress_unstable_features_warning = true
notice.model_migrations."gpt-5.6-terra" = "gpt-6-sol"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
        ),
    )?;
    Ok(())
}

fn write_auth(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;
    Ok(())
}

fn source_turn_sse() -> String {
    responses::sse(vec![
        responses::ev_response_created("resp-handoff-source"),
        responses::ev_assistant_message("msg-handoff-source", SOURCE_RESPONSE),
        responses::ev_completed("resp-handoff-source"),
    ])
}

fn source_turn_with_high_usage_sse() -> String {
    responses::sse(vec![
        responses::ev_response_created("resp-auto-source"),
        responses::ev_assistant_message("msg-auto-source", SOURCE_RESPONSE),
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": "resp-auto-source",
                "usage": {
                    "input_tokens": 85_000,
                    "input_tokens_details": null,
                    "output_tokens": 100,
                    "output_tokens_details": null,
                    "total_tokens": 85_100
                }
            }
        }),
    ])
}

fn handoff_plan_sse() -> String {
    let message =
        format!("The handoff is ready.\n<proposed_plan>\n{HANDOFF_PLAN}\n</proposed_plan>\n");
    responses::sse(vec![
        responses::ev_response_created("resp-handoff-plan"),
        responses::ev_message_item_added("msg-handoff-plan", ""),
        responses::ev_output_text_delta(&message),
        responses::ev_assistant_message("msg-handoff-plan", &message),
        responses::ev_completed("resp-handoff-plan"),
    ])
}

fn execution_turn_sse() -> String {
    responses::sse(vec![
        responses::ev_response_created("resp-handoff-execution"),
        responses::ev_assistant_message("msg-handoff-execution", EXECUTION_RESPONSE),
        responses::ev_completed("resp-handoff-execution"),
    ])
}

fn thread_title_sse(suffix: &str) -> String {
    let response_id = format!("resp-handoff-title-{suffix}");
    let message_id = format!("msg-handoff-title-{suffix}");
    let title = format!(r#"{{"title":"Fresh handoff {suffix}"}}"#);
    responses::sse(vec![
        responses::ev_response_created(&response_id),
        responses::ev_assistant_message(&message_id, &title),
        responses::ev_completed(&response_id),
    ])
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
            rows: 60,
            cols: 160,
        },
        /*inherited_fds*/ &[],
    )
    .await
}

async fn rename_source_thread(
    writer: &tokio::sync::mpsc::Sender<Vec<u8>>,
    output_rx: &mut broadcast::Receiver<Vec<u8>>,
    screen: &mut vt100::Parser,
) -> Result<()> {
    let rename_command = format!("/rename {SOURCE_THREAD_NAME}");
    writer.send(rename_command.as_bytes().to_vec()).await?;
    wait_for_screen(output_rx, screen, "rename command draft", |contents| {
        contents.contains(&rename_command)
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(output_rx, screen, "source thread rename", |contents| {
        // Long macOS temporary paths can truncate the final footer character.
        contents.contains("· sr")
    })
    .await?;
    Ok(())
}

async fn wait_for_request_count(
    response_mock: &responses::ResponseMock,
    expected: usize,
) -> Result<Vec<responses::ResponsesRequest>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 10);
    loop {
        let requests = response_mock.requests();
        if requests.len() >= expected {
            return Ok(requests);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for {expected} model requests; got {}",
                requests.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
}

async fn wait_for_screen(
    output_rx: &mut broadcast::Receiver<Vec<u8>>,
    screen: &mut vt100::Parser,
    label: &str,
    predicate: impl Fn(&str) -> bool,
) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 30);
    loop {
        let contents = screen.screen().contents();
        if predicate(&contents) {
            return Ok(contents);
        }

        let now = tokio::time::Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "timed out waiting for {label}; screen:\n{}",
                screen.screen().contents()
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
                        "timed out waiting for {label} output; screen:\n{}",
                        screen.screen().contents()
                    );
                }
            };
        screen.write_all(&chunk)?;
    }
}
