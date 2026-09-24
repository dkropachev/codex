use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use codex_protocol::ThreadId;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::combine_output_receivers;
use codex_utils_pty::spawn_pty_process;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use tempfile::tempdir;
use tokio::sync::broadcast;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_command_shows_running_status_in_live_tui() -> Result<()> {
    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    let fake_bin = tempdir()?;
    let workflow_release = workspace.path().join("release-workflow");
    let workflow_failure = workspace.path().join("fail-workflow");

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
            r#"model = "gpt-5.6-terra"
model_provider = "openai"
suppress_unstable_features_warning = true

[tui]
status_line = ["thread-id"]

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

    let _workflow_dir = scaffold_test_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "Workflow Test",
    )?;

    let bun_path = fake_bin.path().join("bun");
    std::fs::write(
        &bun_path,
        r##"#!/bin/sh
set -eu
: "${CODEX_TEST_WORKFLOW_RELEASE:?}"
: "${CODEX_TEST_WORKFLOW_FAILURE:?}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config=*|--no-install|--no-env-file) shift ;;
    *) break ;;
  esac
done
case "${2:-}" in
inspect)
  printf '%s\n' '{"apiVersion":1,"id":"code-review","title":"Workflow Test","callableName":"code-review","inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"workingDirectory":{"type":"string"}},"additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"hasComplete":true}'
  exit 0
  ;;
scan)
  printf '%s\n' '[{"path":"src/workflow.ts","exports":["inputSchema","outputSchema"],"imports":[]}]'
  exit 0
  ;;
run) ;;
*)
  printf '%s\n' 'expected shared workflow runner operation' >&2
  exit 64
  ;;
esac
printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":1,"method":"contract","params":{"inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"workingDirectory":{"type":"string"}},"additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
IFS= read -r _contract_ack
while [ ! -f "$CODEX_TEST_WORKFLOW_RELEASE" ]; do
  sleep 0.05
done
if [ -f "$CODEX_TEST_WORKFLOW_FAILURE" ]; then
  printf '%s\n' 'workflow failed for test' >&2
  exit 42
fi
printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":2,"method":"validateOutput","params":{"output":{}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
IFS= read -r _output_ack
printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":0,"method":"complete","params":{"markdown":"# Workflow finished\n\nVisible workflow result.\n"}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
IFS= read -r _completion_ack
"##,
    )?;
    let mut permissions = std::fs::metadata(&bun_path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&bun_path, permissions)?;

    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&existing_path)),
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
        ("PATH".to_string(), path.to_string_lossy().to_string()),
        (
            "CODEX_TEST_WORKFLOW_RELEASE".to_string(),
            workflow_release.display().to_string(),
        ),
        (
            "CODEX_TEST_WORKFLOW_FAILURE".to_string(),
            workflow_failure.display().to_string(),
        ),
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
        /*inherited_fds*/ &[],
    )
    .await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "active thread", |contents| {
        contents
            .split_whitespace()
            .any(|candidate| ThreadId::from_string(candidate).is_ok())
    })
    .await?;

    writer.send(b"/code-review".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "workflow command draft",
        |contents| contents.contains("/code-review"),
    )
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "running workflow status",
        |contents| contents.contains("Working (") && contents.contains("esc to interrupt"),
    )
    .await?;

    std::fs::write(&workflow_release, "release\n")?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "completed workflow output",
        |contents| contents.contains("Visible workflow result.") && !contents.contains("Working ("),
    )
    .await?;

    std::fs::remove_file(&workflow_release)?;
    std::fs::write(&workflow_failure, "fail\n")?;
    writer.send(b"/code-review".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "failing workflow command draft",
        |contents| contents.contains("/code-review"),
    )
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "running workflow status before failure",
        |contents| contents.contains("Working (") && contents.contains("esc to interrupt"),
    )
    .await?;

    std::fs::write(&workflow_release, "release\n")?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "failed workflow output",
        |contents| contents.contains("workflow failed for test") && !contents.contains("Working ("),
    )
    .await?;

    std::fs::remove_file(&workflow_release)?;
    std::fs::remove_file(&workflow_failure)?;
    writer.send(b"/code-review".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "cancelable workflow command draft",
        |contents| contents.contains("/code-review"),
    )
    .await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "running workflow status before cancellation",
        |contents| contents.contains("Working (") && contents.contains("esc to interrupt"),
    )
    .await?;

    writer.send(vec![0x1b]).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "canceled workflow output",
        |contents| contents.contains("Conversation interrupted") && !contents.contains("Working ("),
    )
    .await?;

    spawned.session.terminate();
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workflow_command_autocompletes_in_live_tui() -> Result<()> {
    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let workspace = tempdir()?;
    let fake_bin = tempdir()?;

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
            r#"model = "gpt-5.6-terra"
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

    let _workflow_dir = scaffold_test_workflow(
        &codex_home.path().join("workflows"),
        "review/fix",
        "/code-review",
    )?;
    write_completion_fake_bun(fake_bin.path())?;
    let existing_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&existing_path)),
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
        ("PATH".to_string(), path.to_string_lossy().to_string()),
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
        /*inherited_fds*/ &[],
    )
    .await?;
    let writer = spawned.session.writer_sender();
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.6-terra default")
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
                && contents.contains("--action")
                && contents.contains("--dynamic")
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
                && contents.contains("--action")
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
                && contents.contains("--allowed-areas")
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

fn scaffold_test_workflow(
    root: &std::path::Path,
    id: &str,
    title: &str,
) -> Result<std::path::PathBuf> {
    scaffold_workflow(
        root,
        &ScaffoldRequest {
            id: id.to_string(),
            title: title.to_string(),
            callable_name: "code-review".to_string(),
            description: "Run a code review workflow.".to_string(),
        },
    )
}

#[cfg(unix)]
fn write_completion_fake_bun(root: &std::path::Path) -> Result<()> {
    let path = root.join("bun");
    std::fs::write(
        &path,
        r##"#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
  case "$1" in
    --config=*|--no-install|--no-env-file) shift ;;
    *) break ;;
  esac
done
case "${2:-}" in
  inspect)
    printf '%s\n' '{"apiVersion":1,"id":"review/fix","title":"/code-review","callableName":"code-review","inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"workingDirectory":{"type":"string"},"action":{"description":"Run mode.","enum":["review","list-reports"]},"allowedAreas":{"description":"Allowed areas.","enum":["Test","Code"]}},"additionalProperties":false},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"hasComplete":true,"sources":[]}'
    ;;
  scan)
    printf '%s\n' '[{"path":"src/workflow.ts","exports":["inputSchema","outputSchema"],"imports":[]}]'
    ;;
  complete)
    request=$(cat "${3:?}")
    case "$request" in
      *'"mode":"field"'*) items='[{"value":"--dynamic","description":"Dynamic option."}]' ;;
      *) items='[]' ;;
    esac
    printf '%s\n' "{\"inspection\":{\"apiVersion\":1,\"id\":\"review/fix\",\"title\":\"/code-review\",\"callableName\":\"code-review\",\"inputSchema\":{\"\$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"object\",\"properties\":{\"workingDirectory\":{\"type\":\"string\"},\"action\":{\"description\":\"Run mode.\",\"enum\":[\"review\",\"list-reports\"]},\"allowedAreas\":{\"description\":\"Allowed areas.\",\"enum\":[\"Test\",\"Code\"]}},\"additionalProperties\":false},\"outputSchema\":{\"\$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"type\":\"object\",\"additionalProperties\":true},\"hasComplete\":true},\"items\":$items}"
    ;;
  *)
    printf '%s\n' 'unexpected workflow runner operation' >&2
    exit 64
    ;;
esac
"##,
    )?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
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
