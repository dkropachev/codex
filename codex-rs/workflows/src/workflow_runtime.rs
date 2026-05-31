use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
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

use crate::api_contract::read_published_workflow_api_contract;
use crate::workflow_contract_validation::validate_json_against_schema;
#[cfg(unix)]
use crate::workflow_host;

pub const WORKFLOW_RUNTIME_EVENT_PREFIX: &str = "__CODEX_WORKFLOW_EVENT__";
const WORKFLOW_NAME_ENV: &str = "CODEX_WORKFLOW_NAME";
const WORKFLOW_WORKING_DIRECTORY_ENV: &str = "CODEX_WORKFLOW_WORKING_DIRECTORY";
const WORKFLOW_OUTPUT_FORMAT_ENV: &str = "CODEX_WORKFLOW_OUTPUT_FORMAT";
const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV: &str = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";
const WORKFLOW_APP_SERVER_URL_ENV: &str = "CODEX_WORKFLOW_APP_SERVER_URL";
const WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV: &str =
    "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";

#[derive(Clone, Copy)]
enum WorkflowRuntimeInvocationMode {
    Run,
    Complete,
}

impl WorkflowRuntimeInvocationMode {
    const fn as_str(self) -> &'static str {
        match self {
            WorkflowRuntimeInvocationMode::Run => "run",
            WorkflowRuntimeInvocationMode::Complete => "complete",
        }
    }
}

const WORKFLOW_RUNNER_SOURCE: &str = r#"
import path from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const EVENT_PREFIX = "__CODEX_WORKFLOW_EVENT__";
const WORKFLOW_WORKING_DIRECTORY_ENV = "CODEX_WORKFLOW_WORKING_DIRECTORY";
const WORKFLOW_OUTPUT_FORMAT_ENV = "CODEX_WORKFLOW_OUTPUT_FORMAT";

function parseArgs(argv) {
  let workflowPath;
  let rawInput = "{}";
  let mode = "run";
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--mode") {
      mode = argv[index + 1] ?? "run";
      index += 1;
      continue;
    }
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
  if (mode !== "run" && mode !== "complete") {
    throw new Error(`unsupported --mode ${mode}`);
  }
  return { workflowPath, rawInput, mode };
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

async function emitRequestedFormat(workflowModule, workflow, result) {
  const requestedFormat = stringValue(process.env[WORKFLOW_OUTPUT_FORMAT_ENV]);
  if (!requestedFormat) {
    return;
  }

  if (requestedFormat !== "tui.markdown.v1") {
    if (typeof workflow.format === "function") {
      const formatted = await workflow.format(result, { format: requestedFormat });
      if (requestedFormat === "tui.markdown.v1") {
        const markdown = stringValue(formatted?.markdown);
        if (markdown) {
          emitEvent({ type: "reportToUserMarkdown", markdown });
        }
        return;
      }
    }
    throw new Error(`unsupported host output format ${requestedFormat}`);
  }

  const formatter = workflowModule.WorkflowOutput?.toTuiMarkdown;
  if (typeof formatter === "function") {
    const formatted = await formatter(result);
    const markdown = stringValue(formatted?.markdown);
    if (!markdown) {
      throw new Error(
        `workflow format ${requestedFormat} must return { markdown: string }`,
      );
    }
    emitEvent({ type: "reportToUserMarkdown", markdown });
    return;
  }

  if (typeof workflow.format === "function") {
    const formatted = await workflow.format(result, { format: requestedFormat });
    const markdown = stringValue(formatted?.markdown);
    if (markdown) {
      emitEvent({ type: "reportToUserMarkdown", markdown });
    }
  }
}

function createRuntimeContext() {
  const workflowDirectory = process.cwd();
  const workingDirectory =
    process.env[WORKFLOW_WORKING_DIRECTORY_ENV] ?? workflowDirectory;
  const workflowName = process.env.CODEX_WORKFLOW_NAME ?? path.basename(workflowDirectory);
  const emitStatus = (status) => emitEvent({ type: "status", status: normalizeStatusUpdate(status) });
  return {
    workingDirectory,
    cwd: workingDirectory,
    currentWorkingDirectory: workingDirectory,
    repoRoot: workingDirectory,
    progress: (message, data) => emitStatus(legacyProgressToStatus(workflowName, message, data)),
    status: emitStatus,
    reportToUserMarkdown: (markdown) => emitEvent({ type: "reportToUserMarkdown", markdown }),
  };
}

function normalizeCompletionSuggestion(value) {
  if (typeof value === "string" && value.trim().length > 0) {
    return {
      display: value.trim(),
      insertText: value.trim(),
      description: undefined,
    };
  }

  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("workflow completion entries must be strings or objects");
  }

  const display = stringValue(value.display) ?? stringValue(value.insertText);
  const insertText = stringValue(value.insertText) ?? stringValue(value.display);
  if (!display || !insertText) {
    throw new Error("workflow completion entries require non-empty display or insertText");
  }

  return {
    display,
    insertText,
    description: stringValue(value.description),
  };
}

const { workflowPath, rawInput, mode } = parseArgs(process.argv.slice(2));
const moduleUrl = pathToFileURL(path.resolve(workflowPath)).href;
const workflowModule = await import(moduleUrl);
const workflow = workflowModule.default;

const input = JSON.parse(rawInput ?? "{}");
let output;
if (mode === "complete") {
  if (typeof workflowModule.complete === "function") {
    output = await workflowModule.complete(createRuntimeContext(), input);
  } else if (workflow && typeof workflow.complete === "function") {
    output = await workflow.complete(createRuntimeContext(), input);
  } else {
    output = [];
  }
  output = Array.isArray(output)
    ? output.map(normalizeCompletionSuggestion)
    : [];
} else {
  if (typeof workflow === "function") {
    output = await workflow(createRuntimeContext(), input);
  } else if (workflow && typeof workflow.run === "function") {
    output = await workflow.run(createRuntimeContext(), input);
  } else {
    throw new Error(
      "workflow module must export a named default async function or a default object with a run(ctx, input) method",
    );
  }
  await emitRequestedFormat(workflowModule, workflow, output);
}
process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
"#;

#[derive(Debug)]
pub(crate) struct WorkflowRuntimeOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) success: bool,
    pub(crate) exit_status: String,
}

#[derive(Debug, Clone, Deserialize)]
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

pub type WorkflowRuntimeEventHandler<'a> = dyn Fn(&WorkflowRuntimeEvent) + Send + Sync + 'a;

pub(crate) struct WorkflowRuntimeRunOptions<'a> {
    pub(crate) workflows: &'a [crate::registry::WorkflowSummary],
    pub(crate) event_handler: Option<&'a WorkflowRuntimeEventHandler<'a>>,
    pub(crate) runtime: crate::execute::WorkflowRuntimeContext,
}

struct WorkflowProcessInvocation<'a> {
    codex_home: Option<&'a Path>,
    working_directory: &'a Path,
    workflow_dir: &'a Path,
    workflow_path: &'a Path,
    input: &'a str,
    mode: WorkflowRuntimeInvocationMode,
    runtime: &'a crate::execute::WorkflowRuntimeContext,
    event_handler: Option<&'a WorkflowRuntimeEventHandler<'a>>,
}

pub async fn complete_workflow(
    workflow_dir: &Path,
    working_directory: &Path,
    workflow_path: &Path,
    input: &crate::command::WorkflowCommandInput,
) -> Result<Vec<crate::command_completion::WorkflowCommandCompletionSuggestion>> {
    let runtime = crate::execute::WorkflowRuntimeContext::default();
    let input =
        serde_json::to_string(input).context("failed to serialize workflow completion input")?;
    let output = run_workflow_process(WorkflowProcessInvocation {
        codex_home: None,
        working_directory,
        workflow_dir,
        workflow_path,
        input: &input,
        mode: WorkflowRuntimeInvocationMode::Complete,
        runtime: &runtime,
        event_handler: None,
    })
    .await?;

    if !output.success {
        anyhow::bail!(
            "workflow completion exited with {}: {}",
            output.exit_status,
            output.stderr.trim()
        );
    }

    let suggestions = serde_json::from_str::<
        Vec<crate::command_completion::WorkflowCommandCompletionSuggestion>,
    >(&output.stdout)
    .context("failed to parse workflow completion output")?;
    Ok(suggestions)
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
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    options: WorkflowRuntimeRunOptions<'_>,
) -> Result<WorkflowRuntimeOutput> {
    if !options.runtime.force_process_runtime && workflow_host::should_use_host() {
        let output = workflow_host::run_workflow_via_host(
            codex_home,
            working_directory,
            workflow_dir,
            workflow_path,
            input,
            options.event_handler,
        )
        .await;
        let output = output?;
        validate_workflow_runtime_output(codex_home, options.workflows, workflow_dir, &output)?;
        return Ok(output);
    }

    let output = run_workflow_legacy(
        codex_home,
        working_directory,
        workflow_dir,
        workflow_path,
        input,
        &options.runtime,
        options.event_handler,
    )
    .await?;
    validate_workflow_runtime_output(codex_home, options.workflows, workflow_dir, &output)?;
    Ok(output)
}

#[cfg(not(unix))]
pub(crate) async fn run_workflow(
    codex_home: &Path,
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    options: WorkflowRuntimeRunOptions<'_>,
) -> Result<WorkflowRuntimeOutput> {
    let output = run_workflow_legacy(
        codex_home,
        working_directory,
        workflow_dir,
        workflow_path,
        input,
        &options.runtime,
        options.event_handler,
    )
    .await?;
    validate_workflow_runtime_output(codex_home, options.workflows, workflow_dir, &output)?;
    Ok(output)
}

async fn run_workflow_legacy(
    codex_home: &Path,
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
    runtime: &crate::execute::WorkflowRuntimeContext,
    event_handler: Option<&WorkflowRuntimeEventHandler<'_>>,
) -> Result<WorkflowRuntimeOutput> {
    run_workflow_process(WorkflowProcessInvocation {
        codex_home: Some(codex_home),
        working_directory,
        workflow_dir,
        workflow_path,
        input,
        mode: WorkflowRuntimeInvocationMode::Run,
        runtime,
        event_handler,
    })
    .await
}

fn validate_workflow_runtime_output(
    codex_home: &Path,
    workflows: &[crate::registry::WorkflowSummary],
    workflow_dir: &Path,
    output: &WorkflowRuntimeOutput,
) -> Result<()> {
    if !output.success || output.stdout.trim().is_empty() {
        return Ok(());
    }

    let Some(workflow) = workflows
        .iter()
        .find(|workflow| workflow.path == workflow_dir)
    else {
        return Ok(());
    };

    let Some(contract) = read_published_workflow_api_contract(codex_home, workflow)? else {
        return Ok(());
    };

    let output_json = serde_json::from_str::<JsonValue>(&output.stdout).with_context(|| {
        format!(
            "failed to parse workflow output for {} as JSON",
            workflow.id
        )
    })?;
    validate_json_against_schema(&contract.output_schema, &output_json).with_context(|| {
        format!(
            "workflow output for {} did not match the published contract",
            workflow.id
        )
    })?;

    Ok(())
}

async fn run_workflow_process(
    invocation: WorkflowProcessInvocation<'_>,
) -> Result<WorkflowRuntimeOutput> {
    let runner_path = write_runner_script()?;
    let engine_path = workflow_ts_engine_path(invocation.codex_home, invocation.workflow_dir)?;
    let mut child = Command::new(&engine_path);
    crate::managed_bun::configure_isolated_bun_environment_for_tokio(
        &mut child,
        invocation.codex_home,
    )?;
    child
        .arg(&runner_path)
        .arg("--mode")
        .arg(invocation.mode.as_str())
        .arg("--workflow-path")
        .arg(invocation.workflow_path)
        .arg("--input")
        .arg(invocation.input)
        .env(
            WORKFLOW_WORKING_DIRECTORY_ENV,
            invocation.working_directory.display().to_string(),
        )
        .env(
            WORKFLOW_OUTPUT_FORMAT_ENV,
            invocation
                .runtime
                .output_format
                .clone()
                .or_else(|| env::var(WORKFLOW_OUTPUT_FORMAT_ENV).ok())
                .unwrap_or_default(),
        )
        .current_dir(invocation.workflow_dir)
        .env(
            WORKFLOW_NAME_ENV,
            invocation
                .workflow_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workflow"),
        )
        .env_remove("NODE_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(run_id) = &invocation.runtime.run_id {
        child.env(WORKFLOW_RUN_ID_ENV, run_id);
    }
    if let Some(origin_thread_id) = &invocation.runtime.origin_thread_id {
        child.env(WORKFLOW_ORIGIN_THREAD_ID_ENV, origin_thread_id);
    }
    if let Some(app_server_url) = &invocation.runtime.app_server_url {
        child.env(WORKFLOW_APP_SERVER_URL_ENV, app_server_url);
    }
    if let Some(approvals) = &invocation.runtime.approvals {
        child.env(WORKFLOW_APPROVALS_ENV, approvals);
    }
    if let Some(behavior) = &invocation.runtime.interactive_request_behavior {
        child.env(WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV, behavior);
    }

    let mut child = child.spawn().with_context(|| {
        format!(
            "failed to start workflow runtime for {}",
            invocation.workflow_path.display()
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

    let wait_for_child =
        wait_for_child_with_cancellation(&mut child, invocation.runtime.cancellation_flag.clone());
    let (status, stderr) = tokio::join!(
        wait_for_child,
        read_stderr(stderr, invocation.event_handler)
    );
    let status = status.context("failed to wait for workflow runtime process")?;
    let stderr = stderr?;
    let stdout = stdout_task
        .await
        .context("workflow runtime stdout task panicked")?
        .context("failed to read workflow runtime stdout")?;

    let _ = fs::remove_file(&runner_path);

    Ok(WorkflowRuntimeOutput {
        stdout,
        stderr,
        success: status.success(),
        exit_status: status.to_string(),
    })
}

async fn wait_for_child_with_cancellation(
    child: &mut tokio::process::Child,
    cancellation_flag: Option<Arc<AtomicBool>>,
) -> std::io::Result<std::process::ExitStatus> {
    let Some(cancellation_flag) = cancellation_flag else {
        return child.wait().await;
    };

    tokio::select! {
        status = child.wait() => status,
        () = wait_for_cancellation(&cancellation_flag) => {
            let _ = child.start_kill();
            child.wait().await
        }
    }
}

async fn wait_for_cancellation(cancellation_flag: &AtomicBool) {
    while !cancellation_flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn read_stderr(
    stderr: impl tokio::io::AsyncRead + Unpin,
    event_handler: Option<&WorkflowRuntimeEventHandler<'_>>,
) -> Result<String> {
    let mut reader = BufReader::new(stderr).lines();
    let mut raw_stderr = String::new();
    let forward_runtime_events = event_handler.is_none() && !std::io::stderr().is_terminal();

    while let Some(line) = reader
        .next_line()
        .await
        .context("failed to read workflow runtime stderr")?
    {
        if let Some(payload) = line.strip_prefix(WORKFLOW_RUNTIME_EVENT_PREFIX) {
            match serde_json::from_str::<WorkflowRuntimeEvent>(payload) {
                Ok(event) => {
                    if let Some(event_handler) = event_handler {
                        event_handler(&event);
                    } else if forward_runtime_events {
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

pub(crate) fn workflow_ts_engine_path(
    codex_home: Option<&Path>,
    workflow_dir: &Path,
) -> Result<PathBuf> {
    let bun_path = workflow_bun_path(workflow_dir);
    if bun_path.is_file() {
        return Ok(bun_path);
    }

    match crate::managed_bun::ensure_managed_bun(codex_home) {
        Ok(Some(managed_bun_path)) => return Ok(managed_bun_path),
        Ok(None) => None,
        Err(err) => Some(err),
    }
    .map_or_else(
        || {
            Err(anyhow::anyhow!(
                "workflow runtime requires managed Bun in CODEX_HOME/workflows/.bin or local `{}`",
                bun_path.display()
            ))
        },
        |err| {
            Err(err).with_context(|| {
                format!(
                    "workflow runtime requires managed Bun in CODEX_HOME/workflows/.bin or local `{}`",
                    bun_path.display()
                )
            })
        },
    )
}

pub(crate) fn workflow_bun_path(workflow_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        workflow_dir.join("node_modules/.bin/bun.cmd")
    } else {
        workflow_dir.join("node_modules/.bin/bun")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use pretty_assertions::assert_eq;
    use tempfile::NamedTempFile;
    use tempfile::TempDir;

    use crate::WorkflowCommandInput;

    use super::WORKFLOW_RUNNER_SOURCE;
    use super::WORKFLOW_RUNTIME_EVENT_PREFIX;
    use super::complete_workflow;
    use super::run_workflow_legacy;
    use super::workflow_bun_path;

    #[test]
    fn runner_script_emits_prefixed_events() {
        assert!(WORKFLOW_RUNNER_SOURCE.contains(WORKFLOW_RUNTIME_EVENT_PREFIX));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("reportToUserMarkdown"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("progress"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("status:"));
        assert!(!WORKFLOW_RUNNER_SOURCE.contains("runWorkflow"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("CODEX_WORKFLOW_WORKING_DIRECTORY"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("repoRoot: workingDirectory"));
        assert!(WORKFLOW_RUNNER_SOURCE.contains("mode === \"complete\""));
    }

    #[tokio::test]
    async fn complete_workflow_invokes_complete_hook_and_normalizes_suggestions() {
        let temp = TempDir::new().expect("temp dir");
        let workflow_dir = temp.path().join("code-review");
        let workflow_path = workflow_dir.join("workflow.mjs");
        write_test_workflow(
            &workflow_dir,
            &workflow_path,
            r#"const workflow = {
  async complete(_ctx, input) {
    if (input.text === "--workflow-id") {
      return [
        {
          display: "--workflow-id workflow-123",
          insertText: "--workflow-id workflow-123",
          description: "Pending workflow",
        },
        "--format summary",
      ];
    }
    return [];
  },
  async run() {
    return { workflowStatus: "done" };
  },
};

export default workflow;
"#,
        );

        let suggestions = complete_workflow(
            &workflow_dir,
            &workflow_dir,
            &workflow_path,
            &WorkflowCommandInput {
                argv: vec!["--workflow-id".to_string()],
                text: "--workflow-id".to_string(),
            },
        )
        .await
        .expect("workflow completion should succeed");

        assert_eq!(
            suggestions,
            vec![
                crate::WorkflowCommandCompletionSuggestion {
                    display: "--workflow-id workflow-123".to_string(),
                    insert_text: "--workflow-id workflow-123".to_string(),
                    description: Some("Pending workflow".to_string()),
                },
                crate::WorkflowCommandCompletionSuggestion {
                    display: "--format summary".to_string(),
                    insert_text: "--format summary".to_string(),
                    description: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn complete_workflow_returns_empty_when_hook_is_missing() {
        let temp = TempDir::new().expect("temp dir");
        let workflow_dir = temp.path().join("summary");
        let workflow_path = workflow_dir.join("workflow.mjs");
        write_test_workflow(
            &workflow_dir,
            &workflow_path,
            r#"const workflow = {
  async run() {
    return { workflowStatus: "done" };
  },
};

export default workflow;
"#,
        );

        let suggestions = complete_workflow(
            &workflow_dir,
            &workflow_dir,
            &workflow_path,
            &WorkflowCommandInput {
                argv: Vec::new(),
                text: String::new(),
            },
        )
        .await
        .expect("missing complete hook should return empty suggestions");

        assert!(suggestions.is_empty());
    }

    #[tokio::test]
    async fn complete_workflow_uses_workspace_cwd_for_runtime_context() {
        let temp = TempDir::new().expect("temp dir");
        let workflow_dir = temp.path().join("summary");
        let workspace_cwd = temp.path().join("workspace");
        let workflow_path = workflow_dir.join("workflow.mjs");
        fs::create_dir_all(&workspace_cwd).expect("workspace cwd");
        write_test_workflow(
            &workflow_dir,
            &workflow_path,
            r#"const workflow = {
  async complete(ctx) {
    return [ctx.cwd];
  },
  async run() {
    return { workflowStatus: "done" };
  },
};

export default workflow;
"#,
        );

        let suggestions = complete_workflow(
            &workflow_dir,
            &workspace_cwd,
            &workflow_path,
            &WorkflowCommandInput {
                argv: Vec::new(),
                text: String::new(),
            },
        )
        .await
        .expect("workflow completion should succeed");

        assert_eq!(
            suggestions,
            vec![crate::WorkflowCommandCompletionSuggestion {
                display: workspace_cwd.display().to_string(),
                insert_text: workspace_cwd.display().to_string(),
                description: None,
            }]
        );
    }

    #[tokio::test]
    async fn run_workflow_legacy_kills_runtime_process_when_canceled() {
        let temp = TempDir::new().expect("temp dir");
        let codex_home = temp.path().join("codex-home");
        let workflow_dir = temp.path().join("summary");
        let workflow_path = workflow_dir.join("workflow.mjs");
        fs::create_dir_all(&codex_home).expect("codex home");
        write_test_workflow(
            &workflow_dir,
            &workflow_path,
            r#"const workflow = {
  async run() {
    await new Promise(() => {});
  },
};

export default workflow;
"#,
        );

        let cancellation_flag = Arc::new(AtomicBool::new(true));
        let output = run_workflow_legacy(
            &codex_home,
            temp.path(),
            &workflow_dir,
            &workflow_path,
            "{}",
            &crate::execute::WorkflowRuntimeContext {
                cancellation_flag: Some(Arc::clone(&cancellation_flag)),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("canceled workflow runtime should be reaped");

        assert!(cancellation_flag.load(Ordering::SeqCst));
        assert!(
            !output.success,
            "killed workflow runtime should not report success: {output:?}"
        );
    }

    fn write_test_workflow(workflow_dir: &Path, workflow_path: &Path, source: &str) {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(workflow_dir.join("node_modules/.bin")).expect("workflow dir");
        let mut workflow_file =
            NamedTempFile::new_in(workflow_dir).expect("temporary workflow source");
        std::io::Write::write_all(&mut workflow_file, source.as_bytes()).expect("workflow source");
        workflow_file
            .persist(workflow_path)
            .unwrap_or_else(|err| panic!("persist workflow source: {err}"));

        let bun_path = workflow_bun_path(workflow_dir);
        let mut bun_file = NamedTempFile::new_in(
            bun_path
                .parent()
                .expect("bun wrapper should have a parent directory"),
        )
        .expect("temporary bun wrapper");
        std::io::Write::write_all(&mut bun_file, b"#!/bin/sh\nexec node \"$@\"\n")
            .expect("bun wrapper");
        bun_file
            .persist(&bun_path)
            .unwrap_or_else(|err| panic!("persist bun wrapper: {err}"));
        #[cfg(unix)]
        fs::set_permissions(&bun_path, fs::Permissions::from_mode(0o755)).expect("bun permissions");
    }
}
