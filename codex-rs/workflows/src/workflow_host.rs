use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::registry::WorkflowSummary;
use crate::workflow_runtime::WORKFLOW_RUNTIME_EVENT_PREFIX;
use crate::workflow_runtime::WorkflowRuntimeEvent;
use crate::workflow_runtime::WorkflowRuntimeOutput;
use crate::workflow_runtime::workflow_tsx_path;
use anyhow::Context as _;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::time::sleep;

const WORKFLOW_RUNTIME_MODE_ENV: &str = "CODEX_WORKFLOW_RUNTIME_MODE";
const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV: &str = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";
const WORKFLOW_HOST_RESULT_PREFIX: &str = "__CODEX_WORKFLOW_RESULT__";
const WORKFLOW_HOST_SOCKET_NAME: &str = "runtime.sock";
const WORKFLOW_HOST_SCRIPT_PREFIX: &str = "codex-workflow-host";
const HOST_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_CONNECT_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowHostRequest {
    workflow_path: String,
    workflow_name: String,
    cwd: String,
    input: String,
    run_id: String,
    execution_id: String,
    origin_thread_id: Option<String>,
    registry: Vec<WorkflowHostRegistryEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowHostRegistryEntry {
    id: String,
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowHostResponse {
    stdout: String,
    stderr: String,
    success: bool,
    exit_status: String,
}

#[cfg(unix)]
pub(crate) fn should_use_host() -> bool {
    !matches!(
        env::var(WORKFLOW_RUNTIME_MODE_ENV).ok().as_deref(),
        Some("process")
    )
}

#[cfg(not(unix))]
pub(crate) fn should_use_host() -> bool {
    false
}

#[cfg(unix)]
pub(crate) async fn run_workflow_via_host(
    codex_home: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    workflows: &[WorkflowSummary],
) -> Result<WorkflowRuntimeOutput> {
    let socket_path = workflow_host_socket_path(codex_home);
    ensure_workflow_host(codex_home, workflow_dir, &socket_path).await?;

    let request = WorkflowHostRequest {
        workflow_path: workflow_path.display().to_string(),
        workflow_name: workflow_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workflow")
            .to_string(),
        cwd: workflow_dir.display().to_string(),
        input: input.to_string(),
        run_id: env::var(WORKFLOW_RUN_ID_ENV).unwrap_or_else(|_| {
            format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            )
        }),
        execution_id: format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ),
        origin_thread_id: env::var(WORKFLOW_ORIGIN_THREAD_ID_ENV)
            .ok()
            .filter(|value| !value.is_empty()),
        registry: workflows
            .iter()
            .map(|workflow| WorkflowHostRegistryEntry {
                id: workflow.id.clone(),
                path: workflow.path.display().to_string(),
            })
            .collect(),
    };

    let mut stream = UnixStream::connect(&socket_path).await.with_context(|| {
        format!(
            "failed to connect to workflow host at {}",
            socket_path.display()
        )
    })?;
    let request_json = serde_json::to_string(&request)?;
    stream
        .write_all(request_json.as_bytes())
        .await
        .context("failed to write workflow host request")?;
    stream
        .write_all(b"\n")
        .await
        .context("failed to terminate workflow host request")?;
    stream
        .flush()
        .await
        .context("failed to flush workflow host request")?;

    let mut reader = BufReader::new(stream).lines();
    let mut raw_stderr = String::new();
    let forward_runtime_events = !std::io::stderr().is_terminal();
    let mut response = None;

    while let Some(line) = reader
        .next_line()
        .await
        .context("failed to read workflow host response")?
    {
        if let Some(payload) = line.strip_prefix(WORKFLOW_RUNTIME_EVENT_PREFIX) {
            match serde_json::from_str::<WorkflowRuntimeEvent>(payload) {
                Ok(_) => {
                    if forward_runtime_events {
                        eprintln!("{line}");
                    }
                }
                Err(err) => push_stderr_line(
                    &mut raw_stderr,
                    format!("failed to decode workflow runtime event `{payload}`: {err}"),
                ),
            }
            continue;
        }

        if let Some(payload) = line.strip_prefix(WORKFLOW_HOST_RESULT_PREFIX) {
            response = Some(
                serde_json::from_str::<WorkflowHostResponse>(payload)
                    .context("failed to decode workflow host result")?,
            );
            break;
        }

        push_stderr_line(&mut raw_stderr, line);
    }

    let response =
        response.context("workflow host closed the connection without returning a result")?;
    let stderr = if raw_stderr.is_empty() {
        response.stderr
    } else if response.stderr.is_empty() {
        raw_stderr
    } else {
        format!("{raw_stderr}{}", response.stderr)
    };

    Ok(WorkflowRuntimeOutput {
        stdout: response.stdout,
        stderr,
        success: response.success,
        exit_status: response.exit_status,
    })
}

#[cfg(not(unix))]
pub(crate) async fn run_workflow_via_host(
    _codex_home: &Path,
    _workflow_dir: &Path,
    _workflow_path: &Path,
    _input: &str,
    _workflows: &[WorkflowSummary],
) -> Result<WorkflowRuntimeOutput> {
    Err(anyhow::anyhow!(
        "workflow host is only available on Unix platforms"
    ))
}

#[cfg(unix)]
async fn ensure_workflow_host(
    codex_home: &Path,
    workflow_dir: &Path,
    socket_path: &Path,
) -> Result<()> {
    if connect_to_host(socket_path).await.is_ok() {
        return Ok(());
    }

    if socket_path.exists() {
        let _ = fs::remove_file(socket_path);
    }

    let host_script = write_host_script()?;
    let tsx_path = workflow_tsx_path(workflow_dir);
    if !tsx_path.is_file() {
        return Err(anyhow::anyhow!(
            "workflow host requires local `{}`; global package installs are ignored, so run the workflow install step in this workflow directory before `codex workflow run`",
            tsx_path.display()
        ));
    }

    let mut command = Command::new(&tsx_path);
    command
        .arg(&host_script)
        .arg("--serve")
        .arg(socket_path)
        .current_dir(codex_home)
        .env_remove("NODE_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    command.spawn().with_context(|| {
        format!(
            "failed to start workflow host for {}",
            socket_path.display()
        )
    })?;

    wait_for_host(socket_path).await?;
    let _ = fs::remove_file(&host_script);
    Ok(())
}

#[cfg(unix)]
async fn connect_to_host(socket_path: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket_path).await.with_context(|| {
        format!(
            "failed to connect to workflow host at {}",
            socket_path.display()
        )
    })
}

#[cfg(unix)]
async fn wait_for_host(socket_path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + HOST_CONNECT_TIMEOUT;
    loop {
        match connect_to_host(socket_path).await {
            Ok(stream) => {
                drop(stream);
                return Ok(());
            }
            Err(err) if tokio::time::Instant::now() < deadline => {
                let _ = err;
                sleep(HOST_CONNECT_RETRY).await;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn workflow_host_socket_path(codex_home: &Path) -> PathBuf {
    codex_home.join("workflows").join(WORKFLOW_HOST_SOCKET_NAME)
}

#[cfg(unix)]
fn write_host_script() -> Result<PathBuf> {
    let script_name = format!(
        "{}-{}-{}.mjs",
        WORKFLOW_HOST_SCRIPT_PREFIX,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = env::temp_dir().join(script_name);
    fs::write(&path, WORKFLOW_HOST_SOURCE)
        .with_context(|| format!("failed to write workflow host helper {}", path.display()))?;
    Ok(path)
}

fn push_stderr_line(stderr: &mut String, line: impl AsRef<str>) {
    stderr.push_str(line.as_ref());
    stderr.push('\n');
}

const WORKFLOW_HOST_SOURCE: &str = r##"
import { createServer } from "node:net";
import { readFileSync, unlinkSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";

const EVENT_PREFIX = "__CODEX_WORKFLOW_EVENT__";
const RESULT_PREFIX = "__CODEX_WORKFLOW_RESULT__";
const WORKFLOW_RUN_ID_ENV = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";
const WORKFLOW_NAME_ENV = "CODEX_WORKFLOW_NAME";

function parseArgs(argv) {
  let socketPath;
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--serve") {
      socketPath = argv[index + 1];
      index += 1;
    }
  }
  if (!socketPath) {
    throw new Error("missing --serve <socket-path>");
  }
  return { socketPath };
}

function emitEvent(socket, event) {
  socket.write(`${EVENT_PREFIX}${JSON.stringify(event)}\n`);
}

function createRequestQueue() {
  let tail = Promise.resolve();
  return (task) => {
    const run = tail.then(task, task);
    tail = run.catch(() => {});
    return run;
  };
}

function failureResult(error) {
  return {
    stdout: "",
    stderr: formatError(error),
    success: false,
    exitStatus: "1",
  };
}

function sendResult(socket, result) {
  socket.write(`${RESULT_PREFIX}${JSON.stringify(result)}\n`);
  socket.end();
}

function readRequestLine(socket) {
  return new Promise((resolve, reject) => {
    let buffer = "";

    const cleanup = () => {
      socket.off("data", onData);
      socket.off("error", onError);
      socket.off("end", onEnd);
    };

    const onData = (chunk) => {
      buffer += chunk;
      const newlineIndex = buffer.indexOf("\n");
      if (newlineIndex < 0) {
        return;
      }

      const payload = buffer.slice(0, newlineIndex).trim();
      cleanup();
      resolve(payload);
    };

    const onError = (error) => {
      cleanup();
      reject(error);
    };

    const onEnd = () => {
      const payload = buffer.trim();
      cleanup();
      if (payload.length === 0) {
        resolve(null);
        return;
      }

      resolve(payload);
    };

    socket.on("data", onData);
    socket.once("error", onError);
    socket.once("end", onEnd);
  });
}

function stringValue(value) {
  return typeof value === "string" && value.trim().length > 0 ? value : undefined;
}

function scalarValue(value) {
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return undefined;
}

function normalizeThreadStatus(value, index) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`workflow thread status at index ${index} must be an object`);
  }
  const name = stringValue(value.name);
  const status = stringValue(value.status);
  if (!name || !status) {
    throw new Error(`workflow thread status at index ${index} requires non-empty name and status`);
  }
  return { name, status };
}

function normalizeChildStatus(value, index) {
  const normalized = normalizeStatusUpdate(value, `childStatuses[${index}]`);
  return {
    workflowName: normalized.workflowName,
    workflowStatus: normalized.workflowStatus,
    threads: normalized.threads,
  };
}

function normalizeStatusUpdate(value, label = "status") {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  const workflowName = stringValue(value.workflowName) ?? stringValue(value.name);
  const workflowStatus = stringValue(value.workflowStatus) ?? stringValue(value.status);
  if (!workflowName || !workflowStatus) {
    throw new Error(`${label} requires non-empty workflowName and workflowStatus`);
  }
  const threads = Array.isArray(value.threads)
    ? value.threads.map((thread, index) => normalizeThreadStatus(thread, index))
    : [];
  const childStatuses = Array.isArray(value.childStatuses)
    ? value.childStatuses.map((child, index) => normalizeChildStatus(child, index))
    : [];
  return { workflowName, workflowStatus, threads, childStatuses };
}

function attachChildStatus(status, childStatus) {
  const normalized = normalizeStatusUpdate(status);
  return {
    ...normalized,
    childStatuses: [...normalized.childStatuses, normalizeChildStatus(childStatus, 0)],
  };
}

function formatLegacyProgressData(data) {
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    return scalarValue(data);
  }

  const parts = [];
  const stage = scalarValue(data.stage);
  if (stage) {
    parts.push(stage);
  }

  const step = scalarValue(data.step);
  const total = scalarValue(data.total);
  if (step && total) {
    parts.push(`step ${step}/${total}`);
  } else if (step) {
    parts.push(`step ${step}`);
  }

  for (const [key, value] of Object.entries(data)) {
    if (key === "stage" || key === "step" || key === "total") {
      continue;
    }
    const scalar = scalarValue(value);
    if (scalar) {
      parts.push(`${key} ${scalar}`);
    }
  }

  return parts.length > 0 ? parts.join(", ") : undefined;
}

function legacyProgressToStatus(workflowName, message, data) {
  const trimmedMessage = typeof message === "string" ? message.trim() : "";
  const summary = formatLegacyProgressData(data);
  const workflowStatus = trimmedMessage && summary
    ? `${trimmedMessage} (${summary})`
    : trimmedMessage || summary || "running";
  return {
    workflowName,
    workflowStatus,
    threads: [],
    childStatuses: [],
  };
}

function cacheBustedWorkflowUrl(workflowPath, executionId) {
  const moduleUrl = pathToFileURL(path.resolve(workflowPath)).href;
  const separator = moduleUrl.includes("?") ? "&" : "?";
  return `${moduleUrl}${separator}executionId=${encodeURIComponent(executionId)}`;
}

function captureStderr() {
  const original = process.stderr.write.bind(process.stderr);
  let stderr = "";
  process.stderr.write = (chunk, encoding, callback) => {
    if (typeof chunk === "string") {
      stderr += chunk;
    } else {
      stderr += chunk.toString(typeof encoding === "string" ? encoding : undefined);
    }
    if (typeof callback === "function") {
      callback();
    }
    return true;
  };
  return {
    get value() {
      return stderr;
    },
    restore() {
      process.stderr.write = original;
    },
  };
}

function suppressStdout() {
  const original = process.stdout.write.bind(process.stdout);
  process.stdout.write = (_chunk, _encoding, callback) => {
    if (typeof callback === "function") {
      callback();
    }
    return true;
  };
  return () => {
    process.stdout.write = original;
  };
}

function withWorkflowContext(request, callback) {
  const previous = {
    cwd: process.cwd(),
    runId: process.env[WORKFLOW_RUN_ID_ENV],
    originThreadId: process.env[WORKFLOW_ORIGIN_THREAD_ID_ENV],
    workflowName: process.env[WORKFLOW_NAME_ENV],
    nodePath: process.env.NODE_PATH,
  };

  process.chdir(request.cwd);
  process.env[WORKFLOW_RUN_ID_ENV] = request.runId;
  if (request.originThreadId) {
    process.env[WORKFLOW_ORIGIN_THREAD_ID_ENV] = request.originThreadId;
  } else {
    delete process.env[WORKFLOW_ORIGIN_THREAD_ID_ENV];
  }
  if (request.workflowName) {
    process.env[WORKFLOW_NAME_ENV] = request.workflowName;
  } else {
    delete process.env[WORKFLOW_NAME_ENV];
  }
  delete process.env.NODE_PATH;

  return Promise.resolve(callback()).finally(() => {
    process.chdir(previous.cwd);
    if (previous.runId === undefined) {
      delete process.env[WORKFLOW_RUN_ID_ENV];
    } else {
      process.env[WORKFLOW_RUN_ID_ENV] = previous.runId;
    }
    if (previous.originThreadId === undefined) {
      delete process.env[WORKFLOW_ORIGIN_THREAD_ID_ENV];
    } else {
      process.env[WORKFLOW_ORIGIN_THREAD_ID_ENV] = previous.originThreadId;
    }
    if (previous.workflowName === undefined) {
      delete process.env[WORKFLOW_NAME_ENV];
    } else {
      process.env[WORKFLOW_NAME_ENV] = previous.workflowName;
    }
    if (previous.nodePath === undefined) {
      delete process.env.NODE_PATH;
    } else {
      process.env.NODE_PATH = previous.nodePath;
    }
  });
}

function registryMap(entries) {
  return new Map(entries.map((entry) => [entry.id, entry.path]));
}

function createStatusDispatcher(emit, statusHook) {
  if (typeof statusHook !== "function") {
    return (status) => emit({ type: "status", status: normalizeStatusUpdate(status) });
  }

  return (status) => {
    const normalized = normalizeStatusUpdate(status);
    let reported = false;
    const decision = statusHook(normalized, {
      reportStatus(nextStatus) {
        reported = true;
        emit({ type: "status", status: normalizeStatusUpdate(nextStatus) });
      },
      attachOriginalChildStatus(nextStatus) {
        return attachChildStatus(nextStatus, normalized);
      },
    });

    if (decision === null) {
      return;
    }
    if (decision === undefined) {
      if (!reported) {
        emit({ type: "status", status: normalized });
      }
      return;
    }
    emit({ type: "status", status: normalizeStatusUpdate(decision) });
  };
}

async function executeWorkflowRequest(request, registry, emit, statusHook) {
  const restoreStdout = suppressStdout();
  const stderrCapture = captureStderr();
  try {
    return await withWorkflowContext(request, async () => {
      const workflow = await loadWorkflow(request.workflowPath, request.executionId ?? randomUUID());
      const status = createStatusDispatcher(emit, statusHook);
      const context = createRuntimeContext(request, registry, emit, status);
      const input = JSON.parse(request.input ?? "{}");
      const output = await workflow.run(context, input);
      return {
        stdout: `${JSON.stringify(output, null, 2)}\n`,
        stderr: stderrCapture.value,
        success: true,
        exitStatus: "0",
      };
    });
  } catch (error) {
    const stderr = `${stderrCapture.value}${formatError(error)}`;
    return {
      stdout: "",
      stderr,
      success: false,
      exitStatus: "1",
    };
  } finally {
    stderrCapture.restore();
    restoreStdout();
  }
}

async function loadWorkflow(workflowPath, executionId) {
  const workflowModule = await import(cacheBustedWorkflowUrl(workflowPath, executionId));
  const workflow = workflowModule.default;
  if (!workflow || typeof workflow.run !== "function") {
    throw new Error("workflow module must export a default object with a run(ctx, input) method");
  }
  return workflow;
}

function createRuntimeContext(request, registry, emit, statusSink) {
  const cwd = request.cwd;
  const workflowName = request.workflowName ?? path.basename(cwd);
  return {
    workingDirectory: cwd,
    cwd,
    currentWorkingDirectory: cwd,
    repoRoot: cwd,
    progress(message, data) {
      statusSink(legacyProgressToStatus(workflowName, message, data));
    },
    status(status) {
      statusSink(status);
    },
    reportToUserMarkdown(markdown) {
      emit({ type: "reportToUserMarkdown", markdown });
    },
    async runWorkflow(workflow, input, options) {
      const workflowId = typeof workflow === "string" ? workflow : workflow?.id;
      if (!workflowId || typeof workflowId !== "string") {
        throw new Error("ctx.runWorkflow(workflow, ...) requires a workflow id string or { id }");
      }

      const childPath = registry.get(workflowId);
      if (!childPath) {
        throw new Error(`ctx.runWorkflow(...) could not resolve workflow id ${workflowId}`);
      }

      const childRequest = {
        ...request,
        workflowPath: childPath,
        cwd: path.dirname(childPath),
        workflowName: path.basename(path.dirname(childPath)),
        input: JSON.stringify(input ?? {}),
        executionId: randomUUID(),
      };
      const childStatusHook = typeof options?.onStatusUpdate === "function"
        ? options.onStatusUpdate
        : undefined;
      const childResult = await executeWorkflowRequest(
        childRequest,
        registry,
        emit,
        childStatusHook,
      );
      if (!childResult.success) {
        throw new Error(
          childResult.stderr.trim().length > 0
            ? `child workflow ${workflowId} failed: ${childResult.stderr.trim()}`
            : `child workflow ${workflowId} failed`,
        );
      }
      return childResult.stdout.trim().length > 0 ? JSON.parse(childResult.stdout) : undefined;
    },
  };
}

function formatError(error) {
  if (error instanceof Error) {
    return error.stack || `${error.name}: ${error.message}`;
  }
  return String(error);
}

async function serve(socketPath) {
  await mkdir(path.dirname(socketPath), { recursive: true });
  try {
    unlinkSync(socketPath);
  } catch {
    // Ignore stale or missing sockets.
  }

  const enqueue = createRequestQueue();
  const server = createServer((socket) => {
    socket.setEncoding("utf8");
    void enqueue(async () => {
      try {
        const payload = await readRequestLine(socket);
        if (payload === null) {
          return;
        }
        const request = JSON.parse(payload);
        const registry = registryMap(request.registry ?? []);
        const result = await executeWorkflowRequest(
          request,
          registry,
          (event) => emitEvent(socket, event),
          undefined,
        );
        sendResult(socket, result);
      } catch (error) {
        try {
          sendResult(socket, failureResult(error));
        } catch {
          try {
            socket.end();
          } catch {
            // Ignore secondary failures while closing a broken socket.
          }
        }
      }
    });
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
}

const { socketPath } = parseArgs(process.argv.slice(2));
await serve(socketPath);
"##;
