use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;
use futures::SinkExt;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;

const WORKFLOW_APP_SERVER_URL_ENV: &str = "CODEX_WORKFLOW_APP_SERVER_URL";
const CODEX_APP_SERVER_URL_ENV: &str = "CODEX_APP_SERVER_URL";
const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV: &str = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";
const REMOTE_AUTH_TOKEN_ENV: &str = "CODEX_REMOTE_AUTH_TOKEN";
const EVENT_PREFIX: &str = "__CODEX_WORKFLOW_EVENT__";

const WORKFLOW_RUNNER_SOURCE: &str = r#"
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const EVENT_PREFIX = "__CODEX_WORKFLOW_EVENT__";

function parseArgs(argv) {
  let workflowPath;
  let rawInput = "{}";
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--workflow-path") {
      workflowPath = argv[index + 1];
      index += 1;
      continue;
    }
    if (arg === "--input") {
      rawInput = argv[index + 1] ?? "{}";
      index += 1;
    }
  }
  if (!workflowPath) {
    throw new Error("missing --workflow-path");
  }
  return { workflowPath, rawInput };
}

function hasManagedWorkflowRuntime() {
  return Boolean(process.env.CODEX_WORKFLOW_APP_SERVER_URL || process.env.CODEX_APP_SERVER_URL);
}

function emitEvent(event) {
  process.stderr.write(`${EVENT_PREFIX}${JSON.stringify(event)}\n`);
}

function createRuntimeContext() {
  if (!hasManagedWorkflowRuntime()) {
    return {};
  }
  const cwd = process.cwd();
  return {
    workingDirectory: cwd,
    cwd,
    currentWorkingDirectory: cwd,
    repoRoot: cwd,
    progress: (message, data) => emitEvent({ type: "progress", message, data }),
    reportToUserMarkdown: (markdown) => emitEvent({ type: "reportToUserMarkdown", markdown }),
  };
}

const { workflowPath, rawInput } = parseArgs(process.argv.slice(2));
const moduleUrl = pathToFileURL(path.resolve(workflowPath)).href;
const workflowModule = await import(moduleUrl);
const workflow = workflowModule.default;

if (!workflow || typeof workflow.run !== "function") {
  throw new Error("workflow module must export a default object with a run(ctx, input) method");
}

const input = JSON.parse(rawInput ?? "{}");
const output = await workflow.run(createRuntimeContext(), input);
process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
"#;

#[derive(Debug)]
pub(crate) struct WorkflowRuntimeOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) success: bool,
    pub(crate) exit_status: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WorkflowRuntimeEvent {
    #[serde(rename = "progress")]
    Progress {
        message: String,
        data: Option<JsonValue>,
    },
    #[serde(rename = "reportToUserMarkdown")]
    ReportToUserMarkdown { markdown: String },
}

struct WorkflowNotifier {
    websocket: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    run_id: String,
    thread_id: Option<String>,
    connect_warning: Option<String>,
}

impl WorkflowNotifier {
    async fn from_env() -> Result<Self> {
        let websocket_url = env::var(WORKFLOW_APP_SERVER_URL_ENV)
            .ok()
            .or_else(|| env::var(CODEX_APP_SERVER_URL_ENV).ok())
            .filter(|value| !value.trim().is_empty());
        let run_id = env::var(WORKFLOW_RUN_ID_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(fallback_run_id);
        let thread_id = env::var(WORKFLOW_ORIGIN_THREAD_ID_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty());

        let (websocket, connect_warning) = if let Some(websocket_url) = websocket_url {
            match connect_websocket(&websocket_url, env::var(REMOTE_AUTH_TOKEN_ENV).ok()).await {
                Ok(websocket) => (Some(websocket), None),
                Err(err) => (
                    None,
                    Some(format!(
                        "workflow runtime could not connect to app-server: {err:#}"
                    )),
                ),
            }
        } else {
            (None, None)
        };

        Ok(Self {
            websocket,
            run_id,
            thread_id,
            connect_warning,
        })
    }

    async fn notify(&mut self, event: WorkflowRuntimeEvent) -> Result<()> {
        let Some(websocket) = &mut self.websocket else {
            return Ok(());
        };

        let notification = match event {
            WorkflowRuntimeEvent::Progress { message, data } => json!({
                "jsonrpc": "2.0",
                "method": "workflow/progress",
                "params": WorkflowProgressNotification {
                    run_id: self.run_id.clone(),
                    thread_id: self.thread_id.clone(),
                    message,
                    data,
                }
            }),
            WorkflowRuntimeEvent::ReportToUserMarkdown { markdown } => json!({
                "jsonrpc": "2.0",
                "method": "workflow/reportToUserMarkdown",
                "params": WorkflowMarkdownResultNotification {
                    run_id: self.run_id.clone(),
                    thread_id: self.thread_id.clone(),
                    markdown,
                }
            }),
        };

        websocket
            .send(Message::Text(notification.to_string().into()))
            .await
            .context("failed to send workflow notification frame")?;

        Ok(())
    }

    async fn shutdown(self) -> Result<()> {
        if let Some(mut websocket) = self.websocket {
            let _ = websocket.close(None).await;
        }
        Ok(())
    }
}

pub(crate) async fn run_workflow(
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let runner_path = write_runner_script()?;
    let notifier = WorkflowNotifier::from_env().await?;
    let tsx_path = workflow_tsx_path(workflow_dir);
    if !tsx_path.is_file() {
        return Err(anyhow::anyhow!(
            "workflow runtime requires local `{}`; global package installs are ignored, so run the workflow install step in this workflow directory before `codex workflow run`",
            tsx_path.display()
        ));
    }

    let mut child = Command::new(&tsx_path);
    child
        .arg(&runner_path)
        .arg("--workflow-path")
        .arg(workflow_path)
        .arg("--input")
        .arg(input)
        .current_dir(workflow_dir)
        .env_remove("NODE_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = child.spawn().with_context(|| {
        format!(
            "failed to start workflow runtime for {}",
            workflow_path.display()
        )
    })?;

    let stdout = child
        .stdout
        .take()
        .context("workflow runtime stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("workflow runtime stderr was not piped")?;

    let stdout_task = tokio::spawn(async move {
        let mut stdout_text = String::new();
        BufReader::new(stdout)
            .read_to_string(&mut stdout_text)
            .await
            .map(|_| stdout_text)
    });

    let stderr_task = tokio::spawn(async move { read_stderr(stderr, notifier).await });

    let status = child
        .wait()
        .await
        .context("failed to wait for workflow runtime process")?;
    let stdout = stdout_task
        .await
        .context("workflow runtime stdout task panicked")?
        .context("failed to read workflow runtime stdout")?;
    let stderr = stderr_task
        .await
        .context("workflow runtime stderr task panicked")??;

    let _ = fs::remove_file(&runner_path);

    Ok(WorkflowRuntimeOutput {
        stdout,
        stderr,
        success: status.success(),
        exit_status: status.to_string(),
    })
}

async fn read_stderr(
    stderr: impl tokio::io::AsyncRead + Unpin,
    notifier: WorkflowNotifier,
) -> Result<String> {
    let mut reader = BufReader::new(stderr).lines();
    let mut raw_stderr = String::new();
    let mut notifier = Some(notifier);

    if let Some(warning) = notifier
        .as_ref()
        .and_then(|notifier| notifier.connect_warning.as_ref())
    {
        push_stderr_line(&mut raw_stderr, warning);
    }

    while let Some(line) = reader
        .next_line()
        .await
        .context("failed to read workflow runtime stderr")?
    {
        if let Some(payload) = line.strip_prefix(EVENT_PREFIX) {
            match serde_json::from_str::<WorkflowRuntimeEvent>(payload) {
                Ok(event) => {
                    if let Some(notifier) = notifier.as_mut()
                        && let Err(err) = notifier.notify(event).await
                    {
                        push_stderr_line(
                            &mut raw_stderr,
                            format!("workflow runtime notification failed: {err:#}"),
                        );
                    }
                }
                Err(err) => push_stderr_line(
                    &mut raw_stderr,
                    format!("failed to decode workflow runtime event `{payload}`: {err}"),
                ),
            }
            continue;
        }

        push_stderr_line(&mut raw_stderr, line);
    }

    if let Some(notifier) = notifier.take()
        && let Err(err) = notifier.shutdown().await
    {
        push_stderr_line(
            &mut raw_stderr,
            format!("workflow runtime shutdown failed: {err:#}"),
        );
    }

    Ok(raw_stderr)
}

fn push_stderr_line(stderr: &mut String, line: impl AsRef<str>) {
    stderr.push_str(line.as_ref());
    stderr.push('\n');
}

fn write_runner_script() -> Result<PathBuf> {
    let file_name = format!(
        "codex-workflow-runtime-{}-{}.mjs",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = env::temp_dir().join(file_name);
    fs::write(&path, WORKFLOW_RUNNER_SOURCE)
        .with_context(|| format!("failed to write workflow runtime helper {}", path.display()))?;
    Ok(path)
}

fn workflow_tsx_path(workflow_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        workflow_dir.join("node_modules/.bin/tsx.cmd")
    } else {
        workflow_dir.join("node_modules/.bin/tsx")
    }
}

async fn connect_websocket(
    websocket_url: &str,
    auth_token: Option<String>,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let mut request = websocket_url
        .into_client_request()
        .with_context(|| format!("invalid workflow app-server URL `{websocket_url}`"))?;
    if let Some(auth_token) = auth_token {
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {auth_token}"))
                .context("invalid workflow app-server auth token")?,
        );
    }

    let (mut websocket, _) = connect_async(request).await.with_context(|| {
        format!("failed to open workflow app-server websocket `{websocket_url}`")
    })?;
    websocket
        .send(Message::Text(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "codex-workflow-runtime",
                        "title": null,
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "experimentalApi": true,
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await
        .context("failed to send workflow app-server initialize request")?;

    loop {
        let Some(frame) = websocket.next().await else {
            anyhow::bail!("workflow app-server closed before initialize response");
        };
        match frame.context("failed to read workflow app-server initialize response")? {
            Message::Text(text) => {
                let message: JsonValue = serde_json::from_str(&text)
                    .context("workflow app-server initialize response was not valid JSON")?;
                if message.get("id") == Some(&json!(1)) {
                    break;
                }
            }
            Message::Ping(payload) => websocket
                .send(Message::Pong(payload))
                .await
                .context("failed to respond to workflow app-server ping")?,
            Message::Close(_) => anyhow::bail!("workflow app-server closed during initialize"),
            Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }

    websocket
        .send(Message::Text(
            json!({ "jsonrpc": "2.0", "method": "initialized" })
                .to_string()
                .into(),
        ))
        .await
        .context("failed to send workflow app-server initialized notification")?;

    Ok(websocket)
}

fn fallback_run_id() -> String {
    format!(
        "workflow-runtime-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::EVENT_PREFIX;
    use super::WORKFLOW_RUNNER_SOURCE;

    #[test]
    fn runner_script_emits_prefixed_events() {
        assert!(WORKFLOW_RUNNER_SOURCE.contains(EVENT_PREFIX));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("reportToUserMarkdown"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("progress"));
    }
}
