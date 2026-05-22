use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

#[cfg(unix)]
use crate::workflow_host;

pub const WORKFLOW_RUNTIME_EVENT_PREFIX: &str = "__CODEX_WORKFLOW_EVENT__";
const WORKFLOW_SELF_EXE_ENV: &str = "CODEX_WORKFLOW_SELF_EXE";
const WORKFLOW_NAME_ENV: &str = "CODEX_WORKFLOW_NAME";

const WORKFLOW_RUNNER_SOURCE: &str = r#"
import { spawn } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { createInterface } from "node:readline";
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

function emitEvent(event) {
  process.stderr.write(`${EVENT_PREFIX}${JSON.stringify(event)}\n`);
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

function parseWorkflowEventLine(line, workflowName) {
  if (!line.startsWith(EVENT_PREFIX)) {
    return null;
  }
  const payload = JSON.parse(line.slice(EVENT_PREFIX.length));
  if (payload.type === "status") {
    return { type: "status", status: normalizeStatusUpdate(payload.status) };
  }
  if (payload.type === "progress") {
    return {
      type: "status",
      status: legacyProgressToStatus(workflowName, payload.message, payload.data),
    };
  }
  if (payload.type === "reportToUserMarkdown") {
    return { type: "reportToUserMarkdown", markdown: payload.markdown };
  }
  return null;
}

async function runChildWorkflow(workflow, input, options, emitStatus) {
  const workflowId = typeof workflow === "string" ? workflow : workflow?.id;
  if (!workflowId || typeof workflowId !== "string") {
    throw new Error("ctx.runWorkflow(workflow, ...) requires a workflow id string or { id }");
  }

  const codexExe = process.env.CODEX_WORKFLOW_SELF_EXE;
  if (!codexExe) {
    throw new Error("ctx.runWorkflow(...) requires CODEX_WORKFLOW_SELF_EXE");
  }

  const rawInput = JSON.stringify(input ?? {});
  const child = spawn(
    codexExe,
    ["workflow", "run", workflowId, "--input", rawInput],
    {
      cwd: process.cwd(),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );

  let stdout = "";
  let rawStderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });

  const statusHook = typeof options?.onStatusUpdate === "function"
    ? options.onStatusUpdate
    : undefined;

  const stderrTask = (async () => {
    const lines = createInterface({ input: child.stderr, crlfDelay: Infinity });
    for await (const line of lines) {
      const event = parseWorkflowEventLine(line, workflowId);
      if (!event) {
        rawStderr += `${line}\n`;
        continue;
      }
      if (event.type !== "status") {
        continue;
      }

      const childStatus = event.status;
      if (!statusHook) {
        emitStatus(childStatus);
        continue;
      }

      let reported = false;
      const decision = await statusHook(childStatus, {
        childWorkflowId: workflowId,
        reportStatus: (status) => {
          reported = true;
          emitStatus(normalizeStatusUpdate(status));
        },
        attachOriginalChildStatus: (status) => attachChildStatus(status, childStatus),
      });

      if (decision === null) {
        continue;
      }
      if (decision === undefined) {
        if (!reported) {
          emitStatus(childStatus);
        }
        continue;
      }
      emitStatus(normalizeStatusUpdate(decision));
    }
  })();

  const exitCode = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  await stderrTask;

  if (exitCode !== 0) {
    throw new Error(
      rawStderr.trim().length > 0
        ? `child workflow ${workflowId} failed with ${exitCode}: ${rawStderr.trim()}`
        : `child workflow ${workflowId} failed with ${exitCode}`,
    );
  }

  return stdout.trim().length > 0 ? JSON.parse(stdout) : undefined;
}

function createRuntimeContext() {
  const cwd = process.cwd();
  const workflowName = process.env.CODEX_WORKFLOW_NAME ?? path.basename(cwd);
  const emitStatus = (status) => emitEvent({ type: "status", status: normalizeStatusUpdate(status) });
  return {
    workingDirectory: cwd,
    cwd,
    currentWorkingDirectory: cwd,
    repoRoot: cwd,
    progress: (message, data) => emitStatus(legacyProgressToStatus(workflowName, message, data)),
    status: emitStatus,
    reportToUserMarkdown: (markdown) => emitEvent({ type: "reportToUserMarkdown", markdown }),
    runWorkflow: (workflow, input, options) => runChildWorkflow(workflow, input, options, emitStatus),
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
pub enum WorkflowRuntimeEvent {
    #[serde(rename = "status")]
    Status { status: WorkflowStatusUpdate },
    #[serde(rename = "progress")]
    Progress {
        message: String,
        data: Option<JsonValue>,
    },
    #[serde(rename = "reportToUserMarkdown")]
    ReportToUserMarkdown { markdown: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowThreadStatus {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowChildStatus {
    pub workflow_name: String,
    pub workflow_status: String,
    #[serde(default)]
    pub threads: Vec<WorkflowThreadStatus>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStatusUpdate {
    pub workflow_name: String,
    pub workflow_status: String,
    #[serde(default)]
    pub threads: Vec<WorkflowThreadStatus>,
    #[serde(default)]
    pub child_statuses: Vec<WorkflowChildStatus>,
}

#[cfg(unix)]
pub(crate) async fn run_workflow(
    codex_home: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    workflows: &[crate::registry::WorkflowSummary],
) -> Result<WorkflowRuntimeOutput> {
    if workflow_host::should_use_host() {
        return workflow_host::run_workflow_via_host(
            codex_home,
            workflow_dir,
            workflow_path,
            input,
            workflows,
        )
        .await;
    }

    run_workflow_legacy(workflow_dir, workflow_path, input).await
}

#[cfg(not(unix))]
pub(crate) async fn run_workflow(
    _codex_home: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    _workflows: &[crate::registry::WorkflowSummary],
) -> Result<WorkflowRuntimeOutput> {
    run_workflow_legacy(workflow_dir, workflow_path, input).await
}

async fn run_workflow_legacy(
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let runner_path = write_runner_script()?;
    let tsx_path = workflow_tsx_path(workflow_dir);
    if !tsx_path.is_file() {
        return Err(anyhow::anyhow!(
            "workflow runtime requires local `{}`; global package installs are ignored, so run the workflow install step in this workflow directory before executing the workflow directly",
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
        .env(
            WORKFLOW_SELF_EXE_ENV,
            env::var_os(WORKFLOW_SELF_EXE_ENV)
                .map(PathBuf::from)
                .or_else(|| env::current_exe().ok())
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        )
        .env(
            WORKFLOW_NAME_ENV,
            workflow_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workflow"),
        )
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

    let stderr_task = tokio::spawn(async move { read_stderr(stderr).await });

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

async fn read_stderr(stderr: impl tokio::io::AsyncRead + Unpin) -> Result<String> {
    let mut reader = BufReader::new(stderr).lines();
    let mut raw_stderr = String::new();
    let forward_runtime_events = !std::io::stderr().is_terminal();

    while let Some(line) = reader
        .next_line()
        .await
        .context("failed to read workflow runtime stderr")?
    {
        if let Some(payload) = line.strip_prefix(WORKFLOW_RUNTIME_EVENT_PREFIX) {
            match serde_json::from_str::<WorkflowRuntimeEvent>(payload) {
                Ok(_) => {
                    if forward_runtime_events {
                        eprintln!("{line}");
                    }
                }
                Err(err) => {
                    push_stderr_line(
                        &mut raw_stderr,
                        format!("failed to decode workflow runtime event `{payload}`: {err}"),
                    );
                }
            }
            continue;
        }

        push_stderr_line(&mut raw_stderr, line);
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

pub(crate) fn workflow_tsx_path(workflow_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        workflow_dir.join("node_modules/.bin/tsx.cmd")
    } else {
        workflow_dir.join("node_modules/.bin/tsx")
    }
}

#[cfg(test)]
mod tests {
    use super::WORKFLOW_RUNNER_SOURCE;
    use super::WORKFLOW_RUNTIME_EVENT_PREFIX;

    #[test]
    fn runner_script_emits_prefixed_events() {
        assert!(WORKFLOW_RUNNER_SOURCE.contains(WORKFLOW_RUNTIME_EVENT_PREFIX));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("reportToUserMarkdown"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("progress"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("status:"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("runWorkflow:"));
    }
}
