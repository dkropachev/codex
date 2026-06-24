use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
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

const MCP_ELICITATION_SERVER_PY: &str = r#"
import json
import sys

TOOL = {
    "name": "elicit_boolean",
    "description": "Ask the client for a boolean confirmation through MCP elicitation.",
    "inputSchema": {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    },
    "annotations": {"readOnlyHint": True},
}

ELICITATION_PARAMS = {
    "message": "Allow the MCP live TUI test request?",
    "requestedSchema": {
        "type": "object",
        "properties": {
            "confirmed": {
                "type": "boolean",
                "title": "Confirm",
                "description": "Approve the pending action.",
            }
        },
        "required": ["confirmed"],
    },
}


def read_message():
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line.strip():
            return json.loads(line)


def send_message(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.write(b"\n")
    sys.stdout.buffer.flush()


def respond(message_id, result):
    send_message({"jsonrpc": "2.0", "id": message_id, "result": result})


def respond_error(message_id, code, message):
    send_message({
        "jsonrpc": "2.0",
        "id": message_id,
        "error": {"code": code, "message": message},
    })


def handle_tool_call(message):
    call_id = message["id"]
    params = message.get("params") or {}
    if params.get("name") != "elicit_boolean":
        respond_error(call_id, -32602, "unsupported tool")
        return

    elicitation_id = "elicit-1"
    send_message({
        "jsonrpc": "2.0",
        "id": elicitation_id,
        "method": "elicitation/create",
        "params": ELICITATION_PARAMS,
    })

    while True:
        reply = read_message()
        if reply is None:
            return
        if reply.get("id") == elicitation_id:
            result = reply.get("result") or {}
            respond(call_id, {
                "content": [],
                "structuredContent": {
                    "action": result.get("action"),
                    "content": result.get("content"),
                },
            })
            return
        if "id" in reply and "method" in reply:
            respond_error(reply["id"], -32601, "unsupported request while waiting for elicitation")


def main():
    while True:
        message = read_message()
        if message is None:
            return

        method = message.get("method")
        if method == "initialize":
            respond(message["id"], {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {"listChanged": True}},
                "serverInfo": {
                    "name": "codex-tui-mcp-test-server",
                    "version": "0.0.0",
                },
                "instructions": "Use this server for live TUI MCP tests.",
            })
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            respond(message["id"], {"tools": [TOOL]})
        elif method == "tools/call":
            handle_tool_call(message)
        elif "id" in message:
            respond_error(message["id"], -32601, "unsupported request")


if __name__ == "__main__":
    main()
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_startup_warning_interaction_works_in_live_tui() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let mcp_server = write_mcp_elicitation_server(workspace.path())?;
    let python3 = find_python3()?;
    let server = MockServer::start().await;

    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        Some(McpServerConfig {
            command: python3.display().to_string(),
            args: vec![mcp_server.display().to_string()],
            broken_command: Some("definitely-missing-codex-mcp-live-test-server"),
        }),
    )?;
    write_auth(codex_home.path())?;

    let spawned = spawn_tui(&codex, codex_home.path(), log_dir.path(), workspace.path()).await?;
    let mut output_rx = combine_output_receivers(spawned.stdout_rx, spawned.stderr_rx);
    let mut screen = vt100::Parser::new(/*rows*/ 24, /*cols*/ 80, /*scrollback*/ 0);

    wait_for_screen(&mut output_rx, &mut screen, "composer", |contents| {
        contents.contains("gpt-5.4 default")
    })
    .await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "MCP startup warning",
        |contents| contents.contains("MCP startup incomplete") && contents.contains("broken"),
    )
    .await?;

    spawned.session.terminate();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_elicitation_form_submission_works_in_live_tui() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex = codex_utils_cargo_bin::cargo_bin("codex-tui")?;
    let codex_home = tempdir()?;
    let log_dir = tempdir()?;
    let workspace = tempdir()?;
    let mcp_server = write_mcp_elicitation_server(workspace.path())?;
    let python3 = find_python3()?;
    let server = MockServer::start().await;

    write_config(
        codex_home.path(),
        workspace.path(),
        &server.uri(),
        Some(McpServerConfig {
            command: python3.display().to_string(),
            args: vec![mcp_server.display().to_string()],
            broken_command: None,
        }),
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
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "MCP startup settled",
        |contents| !contents.contains("Booting MCP server"),
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(/*secs*/ 1)).await;
    let function_call_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-mcp-elicit-1"),
            responses::ev_function_call_with_namespace(
                "call-mcp-elicit",
                "mcp__rmcp",
                "elicit_boolean",
                "{}",
            ),
            responses::ev_completed("resp-mcp-elicit-1"),
        ]),
    )
    .await;
    let completion_mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("resp-mcp-elicit-2"),
            responses::ev_assistant_message("msg-mcp-elicit", "mcp elicitation e2e sentinel"),
            responses::ev_completed("resp-mcp-elicit-2"),
        ]),
    )
    .await;

    writer
        .send(b"Trigger MCP elicitation live TUI test".to_vec())
        .await?;
    wait_for_screen(&mut output_rx, &mut screen, "prompt draft", |contents| {
        contents.contains("Trigger MCP elicitation live TUI test")
    })
    .await?;
    writer.send(b"\r".to_vec()).await?;

    let contents = wait_for_screen(
        &mut output_rx,
        &mut screen,
        "MCP elicitation form",
        |contents| {
            (contents.contains("Allow the MCP live TUI test request?")
                && contents.contains("Confirm")
                && contents.contains("True"))
                || contents.contains("mcp elicitation e2e sentinel")
        },
    )
    .await?;
    if contents.contains("mcp elicitation e2e sentinel") {
        spawned.session.terminate();
        let output = completion_mock
            .function_call_output_text("call-mcp-elicit")
            .unwrap_or_else(|| "<missing function_call_output>".to_string());
        let tools = function_call_mock
            .requests()
            .first()
            .map(summarize_response_tools)
            .unwrap_or_else(|| "<missing first request>".to_string());
        anyhow::bail!(
            "assistant response arrived before MCP elicitation form; output: {output}; tools: {tools}"
        );
    }
    writer.send(b"1".to_vec()).await?;
    writer.send(b"\r".to_vec()).await?;
    wait_for_screen(
        &mut output_rx,
        &mut screen,
        "assistant response",
        |contents| contents.contains("mcp elicitation e2e sentinel"),
    )
    .await?;

    spawned.session.terminate();

    let output = completion_mock
        .function_call_output_text("call-mcp-elicit")
        .context("missing MCP function_call_output")?;
    anyhow::ensure!(
        output.contains("\"action\":\"accept\"") && output.contains("\"confirmed\":true"),
        "MCP elicitation result did not include accepted confirmation: {output}"
    );

    Ok(())
}

struct McpServerConfig<'a> {
    command: String,
    args: Vec<String>,
    broken_command: Option<&'a str>,
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
    mcp_server: Option<McpServerConfig<'_>>,
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
approval_policy = "on-request"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[features]
auth_elicitation = true

[projects."{workspace_display}"]
trust_level = "trusted"

[projects."{parent_display}"]
trust_level = "trusted"
"#
    );

    if let Some(mcp_server) = mcp_server {
        let args = mcp_server
            .args
            .iter()
            .map(|arg| format!("\"{}\"", toml_escape(arg)))
            .collect::<Vec<_>>()
            .join(", ");
        config.push_str(&format!(
            r#"
[mcp_servers.rmcp]
command = "{}"
args = [{args}]
startup_timeout_sec = 10
required = true
"#,
            toml_escape(&mcp_server.command)
        ));

        if let Some(broken_command) = mcp_server.broken_command {
            config.push_str(&format!(
                r#"
[mcp_servers.broken]
command = "{broken_command}"
startup_timeout_sec = 1
"#
            ));
        }
    }

    std::fs::write(codex_home.join("config.toml"), config)?;
    Ok(())
}

fn write_mcp_elicitation_server(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("mcp_elicitation_server.py");
    std::fs::write(&path, MCP_ELICITATION_SERVER_PY)?;
    Ok(path)
}

fn find_python3() -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is not set")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("python3");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("python3 not found on PATH")
}

fn summarize_response_tools(request: &responses::ResponsesRequest) -> String {
    let body = request.body_json();
    let Some(tools) = body["tools"].as_array() else {
        return "<missing tools>".to_string();
    };
    tools
        .iter()
        .filter_map(|tool| {
            let tool_type = tool
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let name = tool.get("name").and_then(serde_json::Value::as_str)?;
            Some(format!("{tool_type}:{name}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_auth(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"dummy","tokens":null,"last_refresh":null}"#,
    )?;
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
