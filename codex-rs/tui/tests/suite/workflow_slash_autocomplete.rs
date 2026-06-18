use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use tempfile::tempdir;
use tokio::sync::broadcast;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_command_autocompletes_in_live_tui() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;

    let workspace_display = workspace.path().display();
    let parent_display = workspace
        .path()
        .parent()
        .unwrap_or(workspace.path())
        .display()
        .to_string();
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"model = "gpt-5.4"
model_provider = "openai"
suppress_unstable_features_warning = true

[features]
workflows = true

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
        ),
    )?;
    std::fs::write(
        codex_home.path().join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;

    let workflow_dir = codex_home
        .path()
        .join("workflows")
        .join("review")
        .join("fix");
    std::fs::create_dir_all(&workflow_dir)?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        r#"id: review/fix
command: code-review
title: /code-review
userDescription: Run a code review workflow.
usage:
  options:
    - flag: --action
      valueHint: <review|list-reports>
      description: Run mode.
    - flag: --allowed-areas
      valueHint: <Test|Code>
      description: Allowed areas.
"#,
    )?;

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

    for byte in b"/code-review" {
        writer.send(vec![*byte]).await?;
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    wait_for_screen(&mut output_rx, &mut screen, "workflow popup", |contents| {
        contents.matches("/code-review").count() >= 2
            && contents.contains("Run a code review workflow.")
    })
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow command",
        |contents| {
            contents.contains("/code-review")
                && contents.contains("--action <review|list-reports>")
                && contents.contains("Run mode.")
        },
    )
    .await?;

    for byte in b"--acti" {
        writer.send(vec![*byte]).await?;
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "workflow command option popup",
        |contents| {
            contents.contains("/code-review --acti")
                && contents.contains("--action <review|list-reports>")
                && contents.contains("Run mode.")
        },
    )
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow command option",
        |contents| contents.contains("/code-review --action"),
    )
    .await?;

    for byte in b"list-reports --allo" {
        writer.send(vec![*byte]).await?;
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "workflow second option popup",
        |contents| {
            contents.contains("/code-review --action list-reports --allo")
                && contents.contains("--allowed-areas <Test|Code>")
                && contents.contains("Allowed areas.")
        },
    )
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow second option",
        |contents| contents.contains("/code-review --action list-reports --allowed-areas"),
    )
    .await?;

    writer.send(b"T".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "workflow second option value popup",
        |contents| {
            contents.contains("/code-review --action list-reports --allowed-areas T")
                && contents.contains("--allowed-areas Test")
        },
    )
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow second option value",
        |contents| contents.contains("/code-review --action list-reports --allowed-areas Test"),
    )
    .await?;

    spawned.session.terminate();
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
