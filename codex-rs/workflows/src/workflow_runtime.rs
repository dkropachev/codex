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

pub const WORKFLOW_RUNTIME_EVENT_PREFIX: &str = "__CODEX_WORKFLOW_EVENT__";

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

function emitEvent(event) {
  process.stderr.write(`${EVENT_PREFIX}${JSON.stringify(event)}\n`);
}

function createRuntimeContext() {
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
pub enum WorkflowRuntimeEvent {
    #[serde(rename = "progress")]
    Progress {
        message: String,
        data: Option<JsonValue>,
    },
    #[serde(rename = "reportToUserMarkdown")]
    ReportToUserMarkdown { markdown: String },
}

pub(crate) async fn run_workflow(
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let runner_path = write_runner_script()?;
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

fn workflow_tsx_path(workflow_dir: &Path) -> PathBuf {
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
    }
}
