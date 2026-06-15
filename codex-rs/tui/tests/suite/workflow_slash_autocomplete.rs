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

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = match codex_utils_cargo_bin::cargo_bin("codex") {
        Ok(path) => path,
        Err(_) => {
            let fallback = repo_root.join("codex-rs/target/debug/codex");
            anyhow::ensure!(
                fallback.is_file(),
                "codex binary is unavailable; run `cargo build -p codex-cli` first"
            );
            fallback
        }
    };
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

    let workflow_dir = codex_home.path().join("workflows").join("code-review");
    std::fs::create_dir_all(&workflow_dir)?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: code-review\ncommand: code-review\ntitle: /code-review\nuserDescription: Run a code review workflow.\n",
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

    writer.send(b"/code".to_vec()).await?;
    wait_for_screen(&mut output_rx, &mut screen, "workflow popup", |contents| {
        contents.contains("/code-review") && contents.contains("Run a code review workflow.")
    })
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow command",
        |contents| {
            contents.contains("/code-review") && !contents.contains("Run a code review workflow.")
        },
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(/*secs*/ 10);
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

        let chunk = tokio::time::timeout(deadline.saturating_duration_since(now), output_rx.recv())
            .await
            .with_context(|| format!("timed out waiting for {label} output"))??;
        screen.write_all(&chunk)?;
        raw.extend_from_slice(&chunk);
    }
}
