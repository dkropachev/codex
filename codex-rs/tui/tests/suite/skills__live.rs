use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use core_test_support::responses;
use tempfile::tempdir;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_mention_submits_skill_instructions() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;
    let skill_path = write_skill(
        codex_home.path(),
        "write-haiku",
        "Write compact haiku.",
        "Use a strict haiku form in three lines.",
    )?;

    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        /*disabled_skill_path*/ None,
    )?;
    write_auth(codex_home.path())?;

    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-skill-select"),
            responses::ev_assistant_message("msg-skill-select", "skill selection live sentinel"),
            responses::ev_completed("resp-skill-select"),
        ]),
    )
    .await;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.4 default")
    })
    .await?;

    send_text(&writer, "$write").await?;
    wait_for_screen(&mut output_rx, &mut screen, "skill popup", |contents| {
        contents.contains("$write")
            && contents.contains("write-haiku")
            && contents.contains("Write compact haiku.")
    })
    .await?;

    send_text(&writer, "-haiku").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "complete skill mention",
        |contents| contents.contains("$write-haiku"),
    )
    .await?;

    send_text(&writer, " about live tui").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "prompt with skill mention",
        |contents| {
            contents.contains("$write-haiku about live tui")
                && !contents.contains("Write compact haiku.")
        },
    )
    .await?;

    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "assistant response",
        |contents| contents.contains("skill selection live sentinel"),
    )
    .await?;

    spawned.session.terminate();

    let request = response_mock.single_request();
    anyhow::ensure!(
        request.body_contains_text("about live tui"),
        "submitted prompt did not include draft text: {:?}",
        request.message_input_texts("user")
    );

    let user_texts = request.message_input_texts("user");
    let skill_path = skill_path.to_string_lossy();
    anyhow::ensure!(
        user_texts.iter().any(|text| {
            text.contains("<skill>\n<name>write-haiku</name>")
                && text.contains("<path>")
                && text.contains("Use a strict haiku form in three lines.")
                && text.contains(skill_path.as_ref())
        }),
        "expected write-haiku skill instructions in user input, got {user_texts:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_toggle_enables_disabled_skill_and_preserves_draft() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;
    let disabled_skill_path = write_skill(
        codex_home.path(),
        "disabled-scout",
        "Inspect disabled skill state.",
        "Use the disabled scout instructions after the user enables the skill.",
    )?;
    write_skill(
        codex_home.path(),
        "draft-helper",
        "Keep the mention popup available while a second skill is disabled.",
        "Use draft helper instructions.",
    )?;

    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        Some(&disabled_skill_path),
    )?;
    write_auth(codex_home.path())?;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.4 default")
    })
    .await?;
    writer.send(b"$".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "enabled skill mentions",
        |contents| contents.contains("draft-helper"),
    )
    .await?;
    writer.send(b"\x1b".to_vec()).await?;
    writer.send(vec![0x7f]).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "cleared skill probe",
        |contents| contents.contains("gpt-5.4 default") && !contents.contains('$'),
    )
    .await?;

    send_text(&writer, "/skills").await?;
    wait_for_screen(&mut output_rx, &mut screen, "skills command", |contents| {
        contents.contains("/skills")
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(&mut output_rx, &mut screen, "skills menu", |contents| {
        contents.contains("Skills") && contents.contains("Enable/Disable Skills")
    })
    .await?;

    writer.send(b"\x1b[B".to_vec()).await?;
    tokio::time::sleep(Duration::from_millis(/*millis*/ 50)).await;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "disabled skill toggle row",
        |contents| {
            contents.contains("Enable/Disable Skills") && contents.contains("[ ] disabled-scout")
        },
    )
    .await?;

    send_text(&writer, "disabled").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "filtered disabled skill",
        |contents| contents.contains("> disabled") && contents.contains("[ ] disabled-scout"),
    )
    .await?;
    writer.send(b" ".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "enabled skill row",
        |contents| contents.contains("[x] disabled-scout"),
    )
    .await?;
    writer.send(b"\x1b".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "skill toggle summary",
        |contents| contents.contains("1 skills enabled, 0 skills disabled"),
    )
    .await?;

    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-skill-toggle"),
            responses::ev_assistant_message("msg-skill-toggle", "skill toggle live sentinel"),
            responses::ev_completed("resp-skill-toggle"),
        ]),
    )
    .await;

    send_text(&writer, "draft before $disabled-scout").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "enabled skill mention popup",
        |contents| {
            contents.contains("draft before $disabled-scout")
                && contents.contains("[Skill] Inspect disabled skill state.")
        },
    )
    .await?;
    writer.send(b"\x1b".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "draft preserved after skill popup",
        |contents| {
            contents.contains("draft before $disabled-scout")
                && !contents.contains("[Skill] Inspect disabled skill state.")
        },
    )
    .await?;
    send_text(&writer, " after toggle").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "full draft before submit",
        |contents| contents.contains("draft before $disabled-scout after toggle"),
    )
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "assistant response",
        |contents| contents.contains("skill toggle live sentinel"),
    )
    .await?;

    spawned.session.terminate();

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    anyhow::ensure!(
        !config.contains("disabled-scout") && !config.contains("enabled = false"),
        "expected enabling the skill to remove the disabled override, got:\n{config}"
    );

    let request = response_mock.single_request();
    anyhow::ensure!(
        request.body_contains_text("draft before") && request.body_contains_text("after toggle"),
        "submitted prompt did not preserve surrounding draft text: {:?}",
        request.message_input_texts("user")
    );

    Ok(())
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
        TerminalSize { rows: 24, cols: 80 },
    )
    .await
}

fn write_config(
    codex_home: &Path,
    workspace: &Path,
    server_uri: &str,
    disabled_skill_path: Option<&Path>,
) -> Result<()> {
    let workspace_display = workspace.display();
    let parent_display = workspace
        .parent()
        .unwrap_or(workspace)
        .display()
        .to_string();
    let mut config = format!(
        r#"model = "gpt-5.4"
model_provider = "mock_provider"
suppress_unstable_features_warning = true
approval_policy = "never"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[features]
mentions_v2 = false

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
    );

    if let Some(disabled_skill_path) = disabled_skill_path {
        let escaped_path = disabled_skill_path
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        config.push_str(&format!(
            r#"
[[skills.config]]
path = "{escaped_path}"
enabled = false
"#
        ));
    }

    std::fs::write(codex_home.join("config.toml"), config)?;
    Ok(())
}

fn write_auth(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;
    Ok(())
}

fn write_skill(codex_home: &Path, name: &str, description: &str, body: &str) -> Result<PathBuf> {
    let skill_dir = codex_home.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir)?;
    let path = skill_dir.join("SKILL.md");
    let contents = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    std::fs::write(&path, contents)?;
    Ok(std::fs::canonicalize(&path).unwrap_or(path))
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
        screen.write_all(&chunk)?;
        raw.extend_from_slice(&chunk);
    }
}
