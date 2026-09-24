use std::ffi::OsString;
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

use crate::CompletionItem;
use crate::CompletionRequest;
use crate::WorkflowManifest;

mod process_tree;

use process_tree::WorkflowProcessTree;

/// JavaScript materialized into a run-private file for every workflow operation.
///
/// Keeping the runner embedded lets CLI and hosted execution use exactly the
/// same module loading, schema validation, completion, and formatting logic.
pub const RUNNER_SOURCE: &str = include_str!("runner.js");

pub const WORKFLOW_CONTROL_PREFIX: &str = "\u{1e}CODEX_WORKFLOW_CONTROL ";
pub const WORKFLOW_CONTROL_VERSION: u8 = 1;
pub const MAX_WORKFLOW_CONTROL_FRAME_BYTES: usize = 16 * 1024;
pub const MAX_WORKFLOW_CONTROL_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const WORKFLOW_OUTPUT_MAX_BYTES: usize = 8 * 1024;
pub const MAX_WORKFLOW_COMPLETION_FRAME_BYTES: usize = WORKFLOW_OUTPUT_MAX_BYTES * 6 + 1_024;
pub const MAX_WORKFLOW_RUN_FRAME_BYTES: usize = 1024 * 1024 + 1_024;
pub const MAX_RUNNER_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_COMPLETION_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_COMPLETION_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_INSPECTION_OUTPUT_BYTES: usize = 256 * 1024;
pub const MAX_COMPLETION_OPERATION_OUTPUT_BYTES: usize =
    MAX_INSPECTION_OUTPUT_BYTES + MAX_COMPLETION_OUTPUT_BYTES + MAX_RUNNER_ERROR_BYTES + 1_024;
pub const MAX_RUNNER_ERROR_BYTES: usize = 4 * 1024;
pub const COMPLETION_TIMEOUT: Duration = Duration::from_secs(2);
pub const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const RUNNER_EXIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerOperation {
    Run,
    Inspect,
    Complete,
    Scan,
}

impl RunnerOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Inspect => "inspect",
            Self::Complete => "complete",
            Self::Scan => "scan",
        }
    }
}

/// Run-private files and bounded command-line arguments for the embedded runner.
///
/// The source, payload, and expected manifest live in files so large workflow input never crosses
/// platform command-line or environment-size limits. Keep this value alive until the child exits.
pub struct PreparedRunner {
    _temp_dir: tempfile::TempDir,
    arguments: Vec<OsString>,
}

struct BoundedControlLine {
    bytes: Vec<u8>,
    oversized: bool,
}

struct SyncControlReader<R> {
    reader: R,
    pending: Vec<u8>,
    oversized: bool,
}

struct WorkflowChildGuard {
    child: Child,
    process_tree: WorkflowProcessTree,
    finished: bool,
}

impl WorkflowChildGuard {
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        let (child, process_tree) = WorkflowProcessTree::spawn(command)?;
        Ok(Self {
            child,
            process_tree,
            finished: false,
        })
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.finished = true;
            self.process_tree.terminate();
        }
        Ok(status)
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(/*millis*/ 10));
        }
    }

    fn terminate(&mut self) {
        self.process_tree.terminate();
        if !self.finished && self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.finished = true;
    }
}

impl Drop for WorkflowChildGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl<R: BufRead> SyncControlReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            oversized: false,
        }
    }

    fn next_line(&mut self, maximum_bytes: usize) -> std::io::Result<Option<BoundedControlLine>> {
        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(None);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let content_end = newline.unwrap_or(available.len());
            let remaining = maximum_bytes.saturating_sub(self.pending.len());
            self.pending
                .extend_from_slice(&available[..content_end.min(remaining)]);
            self.oversized |= content_end > remaining;
            self.reader.consume(consumed);
            if newline.is_none() {
                continue;
            }
            if self.pending.last() == Some(&b'\r') {
                self.pending.pop();
            }
            return Ok(Some(BoundedControlLine {
                bytes: std::mem::take(&mut self.pending),
                oversized: std::mem::take(&mut self.oversized),
            }));
        }
    }
}

impl PreparedRunner {
    pub fn new(
        operation: RunnerOperation,
        payload: Option<&str>,
        expected: Option<&WorkflowManifest>,
    ) -> anyhow::Result<Self> {
        let temp_dir = tempfile::tempdir().context("failed to create workflow runner directory")?;
        let runner_path = temp_dir.path().join("runner.mjs");
        fs::write(&runner_path, RUNNER_SOURCE)
            .context("failed to materialize embedded workflow runner")?;
        let payload_path = write_runner_value(temp_dir.path(), "payload.json", payload)?;
        let expected = expected
            .map(serde_json::to_string)
            .transpose()
            .context("failed to serialize expected workflow manifest")?;
        let expected_path = write_runner_value(
            temp_dir.path(),
            "expected-manifest.json",
            expected.as_deref(),
        )?;
        Ok(Self {
            _temp_dir: temp_dir,
            arguments: vec![
                runner_path.into_os_string(),
                OsString::from(operation.as_str()),
                payload_path.into_os_string(),
                expected_path.into_os_string(),
            ],
        })
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

fn write_runner_value(
    root: &Path,
    name: &str,
    value: Option<&str>,
) -> anyhow::Result<std::path::PathBuf> {
    let Some(value) = value else {
        return Ok(Path::new("-").to_path_buf());
    };
    let path = root.join(name);
    fs::write(&path, value).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Construct a standard-library Bun command for the embedded runner.
///
/// The command deliberately leaves stdio and environment configuration to the
/// caller. CLI execution normally inherits stderr so `ctx.progress()` remains
/// visible, while a hosted execution pipes stdio and sets
/// `CODEX_WORKFLOW_CONTROL_PATH`.
pub(crate) fn bun_command(bun: &Path, workflow_dir: &Path, prepared: &PreparedRunner) -> Command {
    let mut command = Command::new(bun);
    command.current_dir(workflow_dir).args(prepared.arguments());
    command
}

/// Run a workflow from the CLI with stdout and stderr inherited by the caller.
///
/// The embedded runner writes only formatted `markdown.v1` output to stdout
/// and writes workflow progress to stderr.
pub fn run_cli_workflow(
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    input: &Value,
) -> anyhow::Result<ExitStatus> {
    let mut stdout = std::io::stdout().lock();
    run_cli_workflow_with_bun(Path::new("bun"), workflow_dir, expected, input, &mut stdout)
}

fn run_cli_workflow_with_bun(
    bun: &Path,
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    input: &Value,
    markdown_writer: &mut impl Write,
) -> anyhow::Result<ExitStatus> {
    if !input.is_object() {
        bail!("workflow input must be a JSON object");
    }
    let payload = serde_json::to_string(input).context("failed to serialize workflow input")?;
    if payload.len() > MAX_RUNNER_INPUT_BYTES {
        bail!("workflow input exceeded {MAX_RUNNER_INPUT_BYTES} bytes");
    }
    let prepared = PreparedRunner::new(RunnerOperation::Run, Some(&payload), Some(expected))?;
    let control_dir = tempfile::tempdir().context("failed to create workflow control directory")?;
    let control_path = control_dir.path().join("control.jsonl");
    let control_file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&control_path)
        .context("failed to create workflow control channel")?;
    let mut command = bun_command(bun, workflow_dir, &prepared);
    command
        .env("CODEX_WORKFLOW_CONTROL_PATH", &control_path)
        .env("CODEX_WORKFLOW_CLI", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = WorkflowChildGuard::spawn(&mut command).with_context(|| {
        format!(
            "failed to run workflow package at {} with {}",
            workflow_dir.display(),
            bun.display()
        )
    })?;
    let mut child_stdin = child
        .child
        .stdin
        .take()
        .context("workflow stdin was not piped")?;
    let mut control_reader = SyncControlReader::new(BufReader::new(control_file));
    let mut contract = None::<crate::WorkflowContract>;
    let mut expected_request_id = 1_u64;
    let mut output_validated = false;
    loop {
        if let Some(line) = control_reader
            .next_line(MAX_WORKFLOW_RUN_FRAME_BYTES + WORKFLOW_CONTROL_PREFIX.len())
            .context("failed to read workflow control channel")?
        {
            if line.oversized {
                bail!("workflow control frame exceeded {MAX_WORKFLOW_RUN_FRAME_BYTES} bytes");
            }
            let encoded = std::str::from_utf8(&line.bytes)
                .context("workflow control frame was not valid UTF-8")?;
            let frame = decode_control_frame(encoded)?;
            let response = match frame.method.as_str() {
                "contract" => {
                    if frame.id != expected_request_id {
                        bail!(
                            "workflow control request id {} was out of order; expected {expected_request_id}",
                            frame.id
                        );
                    }
                    expected_request_id = expected_request_id.saturating_add(1);
                    let built = frame
                        .params
                        .get("inputSchema")
                        .cloned()
                        .zip(frame.params.get("outputSchema").cloned())
                        .context("workflow contract frame omitted inputSchema or outputSchema")
                        .and_then(|(input_schema, output_schema)| {
                            crate::WorkflowContract::from_schemas(input_schema, output_schema)
                        })
                        .and_then(|candidate| {
                            candidate
                                .validate_input(input)
                                .map_err(anyhow::Error::msg)?;
                            Ok(candidate)
                        });
                    match built {
                        Ok(candidate) if contract.is_none() => {
                            contract = Some(candidate);
                            WorkflowControlResponse {
                                v: WORKFLOW_CONTROL_VERSION,
                                id: frame.id,
                                result: Some(Value::Null),
                                error: None,
                            }
                        }
                        Ok(_) => WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id: frame.id,
                            result: None,
                            error: Some("workflow contract was already initialized".to_string()),
                        },
                        Err(err) => WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id: frame.id,
                            result: None,
                            error: Some(format!("{err:#}")),
                        },
                    }
                }
                "validateOutput" => {
                    if frame.id != expected_request_id {
                        bail!(
                            "workflow control request id {} was out of order; expected {expected_request_id}",
                            frame.id
                        );
                    }
                    expected_request_id = expected_request_id.saturating_add(1);
                    match (contract.as_ref(), frame.params.get("output")) {
                        (Some(contract), Some(output)) => match contract.validate_output(output) {
                            Ok(()) => {
                                output_validated = true;
                                WorkflowControlResponse {
                                    v: WORKFLOW_CONTROL_VERSION,
                                    id: frame.id,
                                    result: Some(Value::Null),
                                    error: None,
                                }
                            }
                            Err(error) => WorkflowControlResponse {
                                v: WORKFLOW_CONTROL_VERSION,
                                id: frame.id,
                                result: None,
                                error: Some(error),
                            },
                        },
                        (None, _) => WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id: frame.id,
                            result: None,
                            error: Some("workflow output arrived before its contract".to_string()),
                        },
                        (_, None) => WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id: frame.id,
                            result: None,
                            error: Some(
                                "workflow output validation frame omitted output".to_string(),
                            ),
                        },
                    }
                }
                "requestUserInput" => {
                    if frame.id != expected_request_id {
                        bail!(
                            "workflow control request id {} was out of order; expected {expected_request_id}",
                            frame.id
                        );
                    }
                    expected_request_id = expected_request_id.saturating_add(1);
                    WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id: frame.id,
                        result: None,
                        error: Some(
                            "requestUserInput is only available during hosted workflow execution"
                                .to_string(),
                        ),
                    }
                }
                "complete" => {
                    if contract.is_none() || !output_validated {
                        bail!("workflow completed before contract and output validation");
                    }
                    let payload = encoded
                        .strip_prefix(WORKFLOW_CONTROL_PREFIX)
                        .context("workflow completion frame omitted control prefix")?;
                    if payload.len() > MAX_WORKFLOW_COMPLETION_FRAME_BYTES {
                        bail!(
                            "workflow completion frame exceeded {MAX_WORKFLOW_COMPLETION_FRAME_BYTES} bytes"
                        );
                    }
                    let markdown = crate::parse_completion(payload).map_err(anyhow::Error::msg)?;
                    let response = WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id: frame.id,
                        result: Some(Value::Null),
                        error: None,
                    };
                    child_stdin.write_all(&encode_control_response(&response)?)?;
                    child_stdin.flush()?;
                    let Some(status) = child
                        .wait_for_exit(RUNNER_EXIT_TIMEOUT)
                        .context("failed to wait for workflow runner")?
                    else {
                        child.terminate();
                        bail!(
                            "workflow runner did not exit within {} ms after completion",
                            RUNNER_EXIT_TIMEOUT.as_millis()
                        );
                    };
                    if status.success() {
                        markdown_writer
                            .write_all(markdown.as_bytes())
                            .context("failed to write workflow markdown")?;
                    }
                    return Ok(status);
                }
                method => WorkflowControlResponse {
                    v: WORKFLOW_CONTROL_VERSION,
                    id: frame.id,
                    result: None,
                    error: Some(format!("unsupported workflow control method `{method}`")),
                },
            };
            child_stdin.write_all(&encode_control_response(&response)?)?;
            child_stdin.flush()?;
            continue;
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for workflow runner")?
        {
            if status.success() {
                bail!("workflow command exited without formatted markdown");
            }
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(/*millis*/ 10));
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModuleInspection {
    pub api_version: u32,
    pub id: String,
    pub title: String,
    pub callable_name: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub has_complete: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceInspection {
    pub path: String,
    pub exports: Vec<String>,
    pub imports: Vec<SourceImport>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct SourceImport {
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub builtin: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompletionOperationOutput {
    pub inspection: ModuleInspection,
    pub items: Vec<CompletionItem>,
    pub error: Option<String>,
}

/// Import and inspect a canonical workflow package using the embedded runner.
///
/// This synchronous helper bounds captured output. Callers that require a hard
/// wall-clock limit should use [`PreparedRunner`] with their async process
/// host and apply [`INSPECTION_TIMEOUT`].
pub(crate) fn inspect_workflow(
    workflow_dir: &Path,
    expected: &WorkflowManifest,
) -> anyhow::Result<ModuleInspection> {
    inspect_workflow_with_bun(
        Path::new("bun"),
        workflow_dir,
        expected,
        /*cancelled*/ None,
    )
}

pub(crate) fn scan_workflow_sources(workflow_dir: &Path) -> anyhow::Result<Vec<SourceInspection>> {
    run_json_operation(
        Path::new("bun"),
        workflow_dir,
        RunnerOperation::Scan,
        /*payload*/ None,
        /*expected*/ None,
        MAX_INSPECTION_OUTPUT_BYTES,
        /*cancelled*/ None,
    )
}

fn inspect_workflow_with_bun(
    bun: &Path,
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<ModuleInspection> {
    let inspection = run_json_operation(
        bun,
        workflow_dir,
        RunnerOperation::Inspect,
        /*payload*/ None,
        Some(expected),
        MAX_INSPECTION_OUTPUT_BYTES,
        cancelled,
    )?;
    validate_inspection(&inspection, expected)?;
    Ok(inspection)
}

#[cfg(test)]
pub(crate) fn run_completion_hook(
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    request: &CompletionRequest,
) -> anyhow::Result<Vec<CompletionItem>> {
    Ok(run_completion_operation_with_bun(
        Path::new("bun"),
        workflow_dir,
        expected,
        request,
        /*cancelled*/ None,
    )?
    .items)
}

pub(crate) fn run_completion_operation_cancellable(
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    request: &CompletionRequest,
    cancelled: &AtomicBool,
) -> anyhow::Result<CompletionOperationOutput> {
    run_completion_operation_with_bun(
        Path::new("bun"),
        workflow_dir,
        expected,
        request,
        Some(cancelled),
    )
}

fn run_completion_operation_with_bun(
    bun: &Path,
    workflow_dir: &Path,
    expected: &WorkflowManifest,
    request: &CompletionRequest,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<CompletionOperationOutput> {
    let payload = serde_json::to_string(request)
        .context("failed to serialize workflow completion request")?;
    if payload.len() > MAX_COMPLETION_REQUEST_BYTES {
        bail!("workflow completion request exceeded {MAX_COMPLETION_REQUEST_BYTES} bytes");
    }
    let prepared = PreparedRunner::new(RunnerOperation::Complete, Some(&payload), Some(expected))?;
    let command = bun_command(bun, workflow_dir, &prepared);
    let output: CompletionOperationOutput = run_json_command(
        command,
        RunnerOperation::Complete,
        MAX_COMPLETION_OPERATION_OUTPUT_BYTES,
        cancelled,
    )?;
    validate_inspection(&output.inspection, expected)?;
    Ok(output)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowControlFrame {
    pub v: u8,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowControlResponse {
    pub v: u8,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkflowProgressParams {
    pub message: String,
    pub data: Option<Value>,
}

pub fn decode_control_frame(encoded: &str) -> anyhow::Result<WorkflowControlFrame> {
    let payload = encoded
        .strip_prefix(WORKFLOW_CONTROL_PREFIX)
        .context("workflow control frame did not have the expected prefix")?;
    if payload.len() > MAX_WORKFLOW_RUN_FRAME_BYTES {
        bail!("workflow control frame exceeded {MAX_WORKFLOW_RUN_FRAME_BYTES} bytes");
    }
    let frame = serde_json::from_str::<WorkflowControlFrame>(payload)
        .context("invalid workflow control frame")?;
    if frame.v != WORKFLOW_CONTROL_VERSION {
        bail!(
            "unsupported workflow control version {}; expected {WORKFLOW_CONTROL_VERSION}",
            frame.v
        );
    }
    Ok(frame)
}

pub fn encode_control_response(response: &WorkflowControlResponse) -> anyhow::Result<Vec<u8>> {
    if response.v != WORKFLOW_CONTROL_VERSION {
        bail!(
            "unsupported workflow control version {}; expected {WORKFLOW_CONTROL_VERSION}",
            response.v
        );
    }
    if response.result.is_some() == response.error.is_some() {
        bail!("workflow control response must contain exactly one of result or error");
    }
    let mut encoded =
        serde_json::to_vec(response).context("failed to serialize workflow control response")?;
    if encoded.len() > MAX_WORKFLOW_CONTROL_RESPONSE_BYTES {
        bail!("workflow control response exceeded {MAX_WORKFLOW_CONTROL_RESPONSE_BYTES} bytes");
    }
    encoded.push(b'\n');
    Ok(encoded)
}

fn run_json_operation<T: serde::de::DeserializeOwned>(
    bun: &Path,
    workflow_dir: &Path,
    operation: RunnerOperation,
    payload: Option<&str>,
    expected: Option<&WorkflowManifest>,
    maximum_stdout_bytes: usize,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<T> {
    let prepared = PreparedRunner::new(operation, payload, expected)?;
    run_json_command(
        bun_command(bun, workflow_dir, &prepared),
        operation,
        maximum_stdout_bytes,
        cancelled,
    )
}

fn run_json_command<T: serde::de::DeserializeOwned>(
    command: Command,
    operation: RunnerOperation,
    maximum_stdout_bytes: usize,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<T> {
    let timeout = match operation {
        RunnerOperation::Complete => COMPLETION_TIMEOUT,
        RunnerOperation::Inspect => INSPECTION_TIMEOUT,
        RunnerOperation::Run => INSPECTION_TIMEOUT,
        RunnerOperation::Scan => INSPECTION_TIMEOUT,
    };
    let (status, stdout, stderr, stdout_oversized) =
        run_bounded_command(command, timeout, maximum_stdout_bytes, cancelled)?;
    if !status.success() {
        let details = bounded_utf8(&stderr, MAX_RUNNER_ERROR_BYTES);
        if details.trim().is_empty() {
            bail!(
                "workflow {} failed with status {}",
                operation.as_str(),
                status
            );
        }
        bail!(
            "workflow {} failed with status {}: {}",
            operation.as_str(),
            status,
            details.trim()
        );
    }
    if stdout_oversized {
        bail!(
            "workflow {} output exceeded {maximum_stdout_bytes} bytes",
            operation.as_str()
        );
    }
    serde_json::from_slice(&stdout)
        .with_context(|| format!("workflow {} returned invalid JSON", operation.as_str()))
}

pub(crate) fn run_bounded_command(
    mut command: Command,
    timeout: Duration,
    maximum_stdout_bytes: usize,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<(ExitStatus, Vec<u8>, Vec<u8>, bool)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        WorkflowChildGuard::spawn(&mut command).context("failed to start Bun workflow runner")?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("Bun workflow runner stdout was not piped")?;
    let stderr = child
        .child
        .stderr
        .take()
        .context("Bun workflow runner stderr was not piped")?;
    let stdout = capture_bounded(stdout, maximum_stdout_bytes);
    let stderr = capture_bounded(stderr, MAX_RUNNER_ERROR_BYTES);
    let deadline = Instant::now() + timeout;
    let status = loop {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            child.terminate();
            bail!("workflow runner was cancelled");
        }
        if let Some(status) = child
            .try_wait()
            .context("failed to wait for Bun workflow runner")?
        {
            break status;
        }
        if Instant::now() >= deadline {
            child.terminate();
            bail!("workflow runner timed out after {} ms", timeout.as_millis());
        }
        thread::sleep(Duration::from_millis(10));
    };
    wait_for_capture(&stdout);
    wait_for_capture(&stderr);
    let stdout = capture_snapshot(&stdout);
    let stderr = capture_snapshot(&stderr);
    Ok((status, stdout.bytes, stderr.bytes, stdout.oversized))
}

#[derive(Default)]
struct BoundedCapture {
    bytes: Vec<u8>,
    oversized: bool,
    done: bool,
}

fn capture_bounded(
    mut reader: impl Read + Send + 'static,
    maximum_bytes: usize,
) -> Arc<Mutex<BoundedCapture>> {
    let capture = Arc::new(Mutex::new(BoundedCapture::default()));
    let task_capture = Arc::clone(&capture);
    thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 {
                break;
            }
            let Ok(mut capture) = task_capture.lock() else {
                return;
            };
            let remaining = maximum_bytes.saturating_sub(capture.bytes.len());
            capture
                .bytes
                .extend_from_slice(&buffer[..read.min(remaining)]);
            capture.oversized |= read > remaining;
        }
        if let Ok(mut capture) = task_capture.lock() {
            capture.done = true;
        }
    });
    capture
}

fn wait_for_capture(capture: &Arc<Mutex<BoundedCapture>>) {
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        if capture.lock().map(|capture| capture.done).unwrap_or(true) {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn capture_snapshot(capture: &Arc<Mutex<BoundedCapture>>) -> BoundedCapture {
    capture
        .lock()
        .map(|capture| BoundedCapture {
            bytes: capture.bytes.clone(),
            oversized: capture.oversized,
            done: capture.done,
        })
        .unwrap_or_default()
}

fn validate_inspection(
    inspection: &ModuleInspection,
    expected: &WorkflowManifest,
) -> anyhow::Result<()> {
    if inspection.api_version != expected.api_version {
        bail!(
            "workflow module apiVersion {} does not match workflow.yaml apiVersion {}",
            inspection.api_version,
            expected.api_version
        );
    }
    if inspection.id != expected.id {
        bail!(
            "workflow module id `{}` does not match workflow.yaml id `{}`",
            inspection.id,
            expected.id
        );
    }
    if inspection.title != expected.title {
        bail!(
            "workflow module title `{}` does not match workflow.yaml title `{}`",
            inspection.title,
            expected.title
        );
    }
    if inspection.callable_name != expected.callable_name {
        bail!(
            "workflow module callableName `{}` does not match workflow.yaml callableName `{}`",
            inspection.callable_name,
            expected.callable_name
        );
    }
    Ok(())
}

fn bounded_utf8(bytes: &[u8], maximum_bytes: usize) -> String {
    let mut end = bytes.len().min(maximum_bytes);
    while std::str::from_utf8(&bytes[..end]).is_err() && end > 0 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
