use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
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

const MARKETPLACE_NAME: &str = "codex-curated";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_popup_lists_installed_plugin_and_toggles_enabled_state() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;

    write_marketplace(workspace.path(), &["toggle-plugin"])?;
    write_installed_plugin(
        codex_home.path(),
        "toggle-plugin",
        "Toggle plugin live test.",
        /*with_skill*/ false,
    )?;
    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        &[("toggle-plugin", /*enabled*/ false)],
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

    send_text(&writer, "/plugins").await?;
    wait_for_screen(&mut output_rx, &mut screen, "plugins command", |contents| {
        contents.contains("/plugins")
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(&mut output_rx, &mut screen, "plugin list", |contents| {
        contents.contains("Plugins")
            && contents.contains("[ ] toggle-plugin")
            && contents.contains("Installed 1 of 1")
    })
    .await?;

    writer.send(b" ".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "enabled plugin row",
        |contents| contents.contains("[*] toggle-plugin") && contents.contains("Space to disable"),
    )
    .await?;

    spawned.session.terminate();

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    anyhow::ensure!(
        config.contains(r#"[plugins."toggle-plugin@codex-curated"]"#)
            && config.contains("enabled = true")
            && !config.contains("enabled = false"),
        "expected plugin toggle to persist enabled state, got:\n{config}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_mention_selection_submits_plugin_guidance() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let server = MockServer::start().await;

    write_marketplace(workspace.path(), &["live-plugin"])?;
    write_installed_plugin(
        codex_home.path(),
        "live-plugin",
        "Plugin live mention.",
        /*with_skill*/ true,
    )?;
    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        &[("live-plugin", /*enabled*/ true)],
    )?;
    write_auth(codex_home.path())?;

    let response_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-plugin-mention"),
            responses::ev_assistant_message("msg-plugin-mention", "plugin mention live sentinel"),
            responses::ev_completed("resp-plugin-mention"),
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

    send_text(&writer, "$live").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "plugin mention popup",
        |contents| {
            contents.contains("$live")
                && contents.contains("[Skill]")
                && contents.contains("[Plugin]")
                && contents.contains("live-plugin")
                && contents.contains("codex-curated")
        },
    )
    .await?;

    writer.send(b"\t".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "selected plugin mention",
        |contents| {
            contents.contains("$live-plugin") && !contents.contains("$live-plugin:live-helper")
        },
    )
    .await?;

    send_text(&writer, "please use plugin").await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "prompt with plugin mention",
        |contents| {
            contents.contains("$live-plugin please use plugin")
                && !contents.contains("Plugin live mention.")
        },
    )
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "assistant response",
        |contents| contents.contains("plugin mention live sentinel"),
    )
    .await?;

    spawned.session.terminate();

    let request = response_mock.single_request();
    anyhow::ensure!(
        request.body_contains_text("please use plugin"),
        "submitted prompt did not include draft text: {:?}",
        request.message_input_texts("user")
    );

    let developer_messages = request.message_input_texts("developer");
    anyhow::ensure!(
        developer_messages.iter().any(|text| {
            text.contains("Capabilities from the `live-plugin` plugin:")
                && text.contains("Skills from this plugin are prefixed with `live-plugin:`")
        }),
        "expected explicit plugin guidance in developer messages, got {developer_messages:?}"
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
    plugins: &[(&str, bool)],
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
plugins = true
mentions_v2 = false

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
    );

    for (plugin_name, enabled) in plugins {
        config.push_str(&format!(
            r#"
[plugins."{plugin_name}@{MARKETPLACE_NAME}"]
enabled = {enabled}
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

fn write_marketplace(workspace: &Path, plugin_names: &[&str]) -> Result<()> {
    std::fs::create_dir_all(workspace.join(".git"))?;
    std::fs::create_dir_all(workspace.join(".agents/plugins"))?;

    let plugins = plugin_names
        .iter()
        .map(|plugin_name| {
            format!(
                r#"{{
      "name": "{plugin_name}",
      "source": {{
        "source": "local",
        "path": "./plugins/{plugin_name}"
      }}
    }}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",\n");

    std::fs::write(
        workspace.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{
  "name": "{MARKETPLACE_NAME}",
  "plugins": [
{plugins}
  ]
}}"#
        ),
    )?;
    Ok(())
}

fn write_installed_plugin(
    codex_home: &Path,
    plugin_name: &str,
    short_description: &str,
    with_skill: bool,
) -> Result<()> {
    let plugin_root = codex_home
        .join("plugins/cache")
        .join(MARKETPLACE_NAME)
        .join(plugin_name)
        .join("local");
    let manifest_dir = plugin_root.join(".codex-plugin");
    std::fs::create_dir_all(&manifest_dir)?;
    std::fs::write(
        manifest_dir.join("plugin.json"),
        format!(
            r#"{{
  "name": "{plugin_name}",
  "description": "{short_description}",
  "interface": {{
    "shortDescription": "{short_description}"
  }}
}}"#
        ),
    )?;

    if with_skill {
        let skill_dir = plugin_root.join("skills/live-helper");
        std::fs::create_dir_all(&skill_dir)?;
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: Help with live plugin mention tests.\n---\n\nUse live plugin guidance.\n",
        )?;
    }

    Ok(())
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
