use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::request_user_input::RequestUserInputResponse;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::WORKFLOW_OUTPUT_MAX_BYTES;
use super::truncate_error_output;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

mod host;
mod interaction;

use host::BoundedDiagnostics;
use host::WorkflowControlReader;
use host::WorkflowProcessGroupGuard;
use host::resume_windows_process;
use host::suspend_windows_process;
use interaction::MAX_WORKFLOW_CONTROL_FRAME_BYTES;
use interaction::decode_user_input_request;
use interaction::is_completion;
use interaction::parse_completion;
use interaction::parse_control_request;
use interaction::validate_user_input_response;

const WORKFLOW_CONTROL_PREFIX: &str = "\u{1e}CODEX_WORKFLOW_CONTROL ";
const WORKFLOW_CONTROL_VERSION: u8 = 1;
const MAX_WORKFLOW_CONTROL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_WORKFLOW_COMPLETION_FRAME_BYTES: usize = WORKFLOW_OUTPUT_MAX_BYTES * 6 + 1_024;
const WORKFLOW_EXIT_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 2);

const WORKFLOW_TUI_RUNNER: &str = r#"
const path = await import("node:path");
const fs = await import("node:fs");
const { createInterface } = await import("node:readline");
const { pathToFileURL } = await import("node:url");

const CONTROL_PREFIX = "\u001eCODEX_WORKFLOW_CONTROL ";
const CONTROL_VERSION = 1;
const INPUT_REQUEST_MAX_BYTES = 16 * 1024;
const INPUT_REQUEST_MAX_COUNT = 64;
const OUTPUT_MAX_BYTES = 40 * 1024;
const OUTPUT_TRUNCATION_NOTICE = "\n\n[Workflow output truncated to 40960 bytes.]";
const CONTROL_PATH = process.env.CODEX_WORKFLOW_CONTROL_PATH;
if (!CONTROL_PATH) throw new Error("Workflow control path is unavailable.");
const responseReader = createInterface({ input: process.stdin, crlfDelay: Infinity });
let pendingInputRequest;
let inputClosed = false;
let nextInputRequestId = 1;
let inputRequestQueue = Promise.resolve();

function emitEvent(event) {
  fs.appendFileSync(CONTROL_PATH, `${CONTROL_PREFIX}${JSON.stringify(event)}\n`);
}

function truncateWorkflowOutput(markdown) {
  if (!markdown.endsWith("\n")) markdown += "\n";
  const bytes = Buffer.from(markdown);
  const decoder = new TextDecoder("utf-8", { fatal: true });
  if (bytes.length <= OUTPUT_MAX_BYTES) return decoder.decode(bytes);
  const maxPrefixBytes = OUTPUT_MAX_BYTES - Buffer.byteLength(OUTPUT_TRUNCATION_NOTICE);
  for (let end = maxPrefixBytes; end >= 0; end -= 1) {
    try {
      return decoder.decode(bytes.subarray(0, end)) + OUTPUT_TRUNCATION_NOTICE;
    } catch {}
  }
  return OUTPUT_TRUNCATION_NOTICE;
}

function rejectPendingInputRequest(message) {
  if (pendingInputRequest) {
    pendingInputRequest.reject(new Error(message));
    pendingInputRequest = undefined;
  }
}

responseReader.on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch (error) {
    rejectPendingInputRequest(`Workflow input channel returned invalid JSON: ${error}`);
    return;
  }
  if (!pendingInputRequest || message?.v !== CONTROL_VERSION || message.id !== pendingInputRequest.id) {
    rejectPendingInputRequest("Workflow input channel returned an invalid response frame.");
    return;
  }
  const pending = pendingInputRequest;
  pendingInputRequest = undefined;
  if (typeof message.error === "string") {
    pending.reject(new Error(message.error));
  } else if (Object.prototype.hasOwnProperty.call(message, "result")) {
    pending.resolve(message.result);
  } else {
    pending.reject(new Error("Workflow input response contained neither result nor error."));
  }
});
responseReader.on("close", () => {
  inputClosed = true;
  rejectPendingInputRequest("Workflow input channel closed before a response was received.");
});

function issueUserInputRequest(event) {
  if (inputClosed) return Promise.reject(new Error("Workflow input channel is closed."));
  return new Promise((resolve, reject) => {
    pendingInputRequest = { id: event.id, resolve, reject };
    emitEvent(event);
  });
}

function requestUserInput(params) {
  let paramsSnapshot;
  try {
    const serialized = JSON.stringify(params);
    paramsSnapshot = JSON.parse(serialized);
    const id = nextInputRequestId;
    if (id > INPUT_REQUEST_MAX_COUNT) {
      return Promise.reject(new Error(
        `Workflow exceeded ${INPUT_REQUEST_MAX_COUNT} user input requests.`,
      ));
    }
    const event = {
      v: CONTROL_VERSION,
      id,
      method: "requestUserInput",
      params: paramsSnapshot,
    };
    if (Buffer.byteLength(JSON.stringify(event)) > INPUT_REQUEST_MAX_BYTES) {
      return Promise.reject(new Error(
        `Workflow input request exceeded ${INPUT_REQUEST_MAX_BYTES} bytes.`,
      ));
    }
    nextInputRequestId += 1;
    const request = inputRequestQueue.then(() => issueUserInputRequest(event));
    inputRequestQueue = request.then(() => undefined, () => undefined);
    return request;
  } catch (error) {
    return Promise.reject(new Error(`Workflow input request must be JSON-serializable: ${error}`));
  }
}

const rawInput = process.argv[1] ?? "{}";
const input = JSON.parse(rawInput);
const workflowModule = await import(pathToFileURL(path.join(process.cwd(), "src/workflow.ts")).href);
const workflow = workflowModule.default ?? workflowModule;
if (!workflow || typeof workflow.run !== "function" || typeof workflow.format !== "function") {
  throw new Error("Workflow must export run() and format().");
}

const context = { progress: () => {}, requestUserInput };
if (input && typeof input === "object" && typeof input.workingDirectory === "string") {
  context.workingDirectory = input.workingDirectory;
  context.cwd = input.workingDirectory;
  context.currentWorkingDirectory = input.workingDirectory;
  context.repoRoot = input.workingDirectory;
}

try {
  const result = await workflow.run(context, input);
  await inputRequestQueue;
  const formatted = await workflow.format(result, { format: "tui.markdown.v1" });
  if (!formatted || typeof formatted.markdown !== "string") {
    throw new Error("Workflow formatter did not return markdown for tui.markdown.v1.");
  }
  await new Promise((resolve, reject) => {
    pendingInputRequest = { id: 0, resolve, reject };
    emitEvent({
      v: CONTROL_VERSION,
      id: 0,
      method: "complete",
      params: { markdown: truncateWorkflowOutput(formatted.markdown) },
    });
  });
} finally {
  responseReader.close();
}
"#;

pub(super) async fn run_workflow_for_tui(
    workflow_dir: &Path,
    input: &Value,
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    cancellation_token: &CancellationToken,
) -> Result<Option<String>, String> {
    let input_json = serde_json::to_string(input)
        .map_err(|err| format!("failed to serialize workflow input: {err}"))?;
    let control_dir = tempfile::tempdir()
        .map_err(|err| format!("failed to create workflow control directory: {err}"))?;
    let control_path = control_dir.path().join("control.jsonl");
    let control_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&control_path)
        .map_err(|err| format!("failed to create workflow control channel: {err}"))?;
    let control_reader = control_file
        .try_clone()
        .map_err(|err| format!("failed to clone workflow control channel: {err}"))?;
    let mut command = Command::new("bun");
    command
        .current_dir(workflow_dir)
        .arg("--eval")
        .arg(WORKFLOW_TUI_RUNNER)
        .arg("--")
        .arg(input_json)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    suspend_windows_process(&mut command);
    command.env("CODEX_WORKFLOW_CONTROL_PATH", &control_path);
    #[cfg(target_os = "linux")]
    let parent_pid = unsafe { libc::getpid() };
    #[cfg(unix)]
    unsafe {
        command.pre_exec(move || {
            codex_utils_pty::process_group::detach_from_tty()?;
            #[cfg(target_os = "linux")]
            codex_utils_pty::process_group::set_parent_death_signal(parent_pid)?;
            Ok(())
        });
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to start workflow command: {err}"))?;
    let process_guard = WorkflowProcessGroupGuard::new(&child)?;
    resume_windows_process(&child)?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "workflow command stdin was not piped".to_string())?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| "workflow command stderr was not piped".to_string())?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "workflow command stdout was not piped".to_string())?;
    let mut child_wait = Box::pin(child.wait());
    let mut control_reader = WorkflowControlReader::new(tokio::fs::File::from_std(control_reader));
    let stderr_diagnostics = BoundedDiagnostics::new(child_stderr);
    let stdout_diagnostics = BoundedDiagnostics::new(child_stdout);
    let mut expected_request_id = 1_u64;

    loop {
        let line = tokio::select! {
            () = cancellation_token.cancelled() => return Ok(None),
            line = control_reader.next_line(
                MAX_WORKFLOW_COMPLETION_FRAME_BYTES + WORKFLOW_CONTROL_PREFIX.len(),
            ) => line.map_err(|err| format!("failed to read workflow control channel: {err}"))?,
            status = &mut child_wait => {
                process_guard.terminate();
                let stderr = stderr_diagnostics.finish().await;
                let stdout = stdout_diagnostics.finish().await;
                return Err(workflow_exit_error(status, &stderr, &stdout));
            }
        };
        if line.bytes.starts_with(WORKFLOW_CONTROL_PREFIX.as_bytes()) {
            let payload = std::str::from_utf8(&line.bytes[WORKFLOW_CONTROL_PREFIX.len()..])
                .map_err(|err| format!("workflow control frame was not valid UTF-8: {err}"))?;
            if is_completion(payload)? {
                if line.oversized {
                    return Err(format!(
                        "workflow completion frame exceeded {MAX_WORKFLOW_COMPLETION_FRAME_BYTES} bytes"
                    ));
                }
                let markdown = parse_completion(payload)?;
                if cancellation_token.is_cancelled() {
                    return Ok(None);
                }
                write_control_message(
                    &mut child_stdin,
                    &json!({
                        "v": WORKFLOW_CONTROL_VERSION,
                        "id": 0,
                        "result": null,
                    }),
                )
                .await?;
                let status = tokio::select! {
                    () = cancellation_token.cancelled() => return Ok(None),
                    status = &mut child_wait => Some(status),
                    () = tokio::time::sleep(WORKFLOW_EXIT_TIMEOUT) => None,
                };
                let Some(status) = status else {
                    process_guard.terminate();
                    let _ = tokio::time::timeout(WORKFLOW_EXIT_TIMEOUT, &mut child_wait).await;
                    return Ok(Some(markdown));
                };
                match status {
                    Ok(status) if status.success() => {}
                    status => {
                        process_guard.terminate();
                        let stderr = stderr_diagnostics.finish().await;
                        let stdout = stdout_diagnostics.finish().await;
                        return Err(workflow_exit_error(status, &stderr, &stdout));
                    }
                }
                return Ok(Some(markdown));
            }
            if line.oversized || payload.len() > MAX_WORKFLOW_CONTROL_FRAME_BYTES {
                return Err(format!(
                    "workflow control frame exceeded {MAX_WORKFLOW_CONTROL_FRAME_BYTES} bytes"
                ));
            }
            let request = parse_control_request(payload, expected_request_id)?;
            expected_request_id = expected_request_id.saturating_add(1);

            let args = decode_user_input_request(request.params);
            let args = match args {
                Ok(args) => args,
                Err(error) => {
                    write_control_message(
                        &mut child_stdin,
                        &json!({
                            "v": WORKFLOW_CONTROL_VERSION,
                            "id": request.id,
                            "error": error,
                        }),
                    )
                    .await?;
                    continue;
                }
            };

            let call_id = workflow_user_input_call_id(&turn_context.sub_id, request.id);
            let response_contract = args.clone();
            let response = session.request_user_input(turn_context.as_ref(), call_id, args);
            tokio::pin!(response);
            let response = tokio::select! {
                response = &mut response => response,
                status = &mut child_wait => {
                    process_guard.terminate();
                    let stderr = stderr_diagnostics.finish().await;
                    let stdout = stdout_diagnostics.finish().await;
                    return Err(workflow_exited_while_waiting(status, &stderr, &stdout));
                }
                line = control_reader.next_line(
                    MAX_WORKFLOW_COMPLETION_FRAME_BYTES + WORKFLOW_CONTROL_PREFIX.len(),
                ) => {
                    let line = line.map_err(|err| {
                        format!("failed to read workflow control channel: {err}")
                    })?;
                    if line.bytes.starts_with(WORKFLOW_CONTROL_PREFIX.as_bytes()) {
                        return Err(
                            "workflow emitted a concurrent user input control request".to_string(),
                        );
                    }
                    return Err("workflow control channel contained an invalid frame".to_string());
                }
                () = cancellation_token.cancelled() => return Ok(None),
            };
            let Some(response) = response else {
                return Ok(None);
            };
            match validate_user_input_response(&response_contract, response) {
                Ok(response) => {
                    write_user_input_response(&mut child_stdin, request.id, &response).await?;
                }
                Err(error) => {
                    write_control_message(
                        &mut child_stdin,
                        &json!({
                            "v": WORKFLOW_CONTROL_VERSION,
                            "id": request.id,
                            "error": error,
                        }),
                    )
                    .await?;
                }
            }
        } else {
            return Err("workflow control channel contained an invalid frame".to_string());
        }
    }
}

fn workflow_user_input_call_id(turn_id: &str, request_id: u64) -> String {
    format!("workflow-user-input-{turn_id}-{request_id}")
}

async fn write_user_input_response(
    child_stdin: &mut ChildStdin,
    request_id: u64,
    response: &RequestUserInputResponse,
) -> Result<(), String> {
    let answers = response.answers.iter().collect::<BTreeMap<_, _>>();
    write_control_message(
        child_stdin,
        &json!({
            "v": WORKFLOW_CONTROL_VERSION,
            "id": request_id,
            "result": { "answers": answers },
        }),
    )
    .await
}

async fn write_control_message(
    child_stdin: &mut ChildStdin,
    message: &Value,
) -> Result<(), String> {
    let encoded = encode_control_message(message)?;
    child_stdin
        .write_all(&encoded)
        .await
        .map_err(|err| format!("failed to write workflow control response: {err}"))?;
    child_stdin
        .flush()
        .await
        .map_err(|err| format!("failed to flush workflow control response: {err}"))
}

fn encode_control_message(message: &Value) -> Result<Vec<u8>, String> {
    let mut encoded = serde_json::to_vec(message)
        .map_err(|err| format!("failed to serialize workflow control response: {err}"))?;
    if encoded.len() > MAX_WORKFLOW_CONTROL_RESPONSE_BYTES {
        return Err(format!(
            "workflow control response exceeded {MAX_WORKFLOW_CONTROL_RESPONSE_BYTES} bytes"
        ));
    }
    encoded.push(b'\n');
    Ok(encoded)
}

fn workflow_exited_while_waiting(
    status: io::Result<ExitStatus>,
    stderr: &[u8],
    stdout: &[u8],
) -> String {
    let message = match status {
        Ok(status) => {
            format!("workflow command exited with status {status} while waiting for user input")
        }
        Err(err) => format!("failed to wait for workflow command: {err}"),
    };
    let details = workflow_failure_details(stderr, stdout);
    if details.is_empty() {
        message
    } else {
        format!("{message}: {details}")
    }
}

fn workflow_exit_error(status: io::Result<ExitStatus>, stderr: &[u8], stdout: &[u8]) -> String {
    let status = match status {
        Ok(status) if status.success() => {
            return "workflow command exited without formatted markdown".to_string();
        }
        Ok(status) => status,
        Err(err) => return format!("failed to wait for workflow command: {err}"),
    };
    let details = workflow_failure_details(stderr, stdout);
    format!(
        "workflow command failed with status {}: {details}",
        exit_status_label(status)
    )
}

fn workflow_failure_details(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    truncate_error_output(details)
}

fn exit_status_label(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    )
}

#[cfg(test)]
#[path = "runtime/runner_tests.rs"]
mod tests;
