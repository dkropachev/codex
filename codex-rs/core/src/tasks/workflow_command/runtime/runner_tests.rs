use std::fs::OpenOptions;
use std::process::Stdio;
use std::time::Duration;

use codex_utils_path_uri::PathUri;
use codex_workflows::normalize_workflow_input_with_working_directory;
use codex_workflows::runner::MAX_WORKFLOW_COMPLETION_FRAME_BYTES;
use codex_workflows::runner::MAX_WORKFLOW_CONTROL_RESPONSE_BYTES;
use codex_workflows::runner::PreparedRunner;
use codex_workflows::runner::RunnerOperation;
use codex_workflows::runner::WORKFLOW_CONTROL_PREFIX;
use codex_workflows::runner::WORKFLOW_CONTROL_VERSION;
use codex_workflows::runner::WORKFLOW_OUTPUT_MAX_BYTES;
use codex_workflows::runner::WorkflowControlResponse;
use codex_workflows::runner::encode_control_response;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::fs::File as TokioFile;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::BoundedLine;
use super::WorkflowControlEvent;
use super::WorkflowControlReader;
#[cfg(windows)]
use super::WorkflowProcessGroupGuard;
use super::decode_workflow_control_event;
#[cfg(windows)]
use super::resume_windows_process;
#[cfg(windows)]
use super::suspend_windows_process;
use super::workflow_failure_details;
use super::workflow_user_input_call_id;

const RUNNER_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);

#[tokio::test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
async fn bun_runner_serializes_concurrent_user_input_requests() {
    let bun = which::which("bun").expect("workflow runtime tests require Bun");

    let temp_dir = tempfile::tempdir().expect("create workflow directory");
    let source_dir = temp_dir.path().join("src");
    std::fs::create_dir(&source_dir).expect("create workflow source directory");
    std::fs::write(
        source_dir.join("workflow.ts"),
        r#"export interface WorkflowInput { [key: string]: unknown }
export interface WorkflowOutput { [key: string]: unknown }
export const inputSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  additionalProperties: true,
};
export const outputSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  additionalProperties: true,
};
export default {
  apiVersion: 1,
  id: "runtime-test",
  title: "Runtime Test",
  callableName: "runtime-test",
  inputSchema,
  outputSchema,
  async run(ctx) {
    const firstRequest = { questions: [{ id: "first", header: "First", question: "First?", options: [{ label: "A", description: "A." }, { label: "B", description: "B." }] }] };
    const firstRequestPromise = ctx.requestUserInput(firstRequest);
    firstRequest.questions[0].question = "Mutated after request";
    const [first, second] = await Promise.all([
      firstRequestPromise,
      ctx.requestUserInput({ questions: [{ id: "second", header: "Second", question: "Second?" }] }),
    ]);
    let oversizedError;
    try {
      await ctx.requestUserInput({ questions: [], padding: "x".repeat(17_000) });
    } catch (error) {
      oversizedError = error.message;
    }
    const third = await ctx.requestUserInput({ questions: [{ id: "third", header: "Third", question: "Third?" }] });
    for (let id = 4; id <= 64; id += 1) {
      await ctx.requestUserInput({ questions: [{ id: `q_${id}`, header: "More", question: `Question ${id}?` }] });
    }
    let requestLimitError;
    try {
      await ctx.requestUserInput({ questions: [{ id: "q_65", header: "More", question: "Question 65?" }] });
    } catch (error) {
      requestLimitError = error.message;
    }
    return { first, second, oversizedError, third, requestLimitError };
  },
  format(result, options) {
    if (options.format !== "markdown.v1") throw new Error("unexpected format");
    return { markdown: JSON.stringify(result) + "\ud800" + "é".repeat(30_000) };
  },
};
"#,
    )
    .expect("write workflow source");

    let control_dir = tempfile::tempdir().expect("create control directory");
    let control_path = control_dir.path().join("control.jsonl");
    let control_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&control_path)
        .expect("create control file");
    let control_reader = control_file.try_clone().expect("clone control file");

    let prepared_runner =
        PreparedRunner::new(RunnerOperation::Run, Some("{}"), /*expected*/ None)
            .expect("prepare workflow runner");
    let mut child = Command::new(&bun)
        .current_dir(temp_dir.path())
        .args(prepared_runner.arguments())
        .env("CODEX_WORKFLOW_CONTROL_PATH", &control_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start Bun workflow runner");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut control_reader = WorkflowControlReader::new(TokioFile::from_std(control_reader));
    acknowledge_contract(&mut control_reader, &mut stdin).await;

    let first = read_control_frame(&mut control_reader).await;
    assert_eq!(
        first,
        json!({
            "v": 1,
            "id": 2,
            "method": "requestUserInput",
            "params": {
                "questions": [{
                    "id": "first",
                    "header": "First",
                    "question": "First?",
                    "options": [
                        { "label": "A", "description": "A." },
                        { "label": "B", "description": "B." },
                    ],
                }],
            },
        })
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(/*millis*/ 100),
            read_control_frame(&mut control_reader)
        )
        .await
        .is_err(),
        "second request must wait for the first response"
    );
    write_response(&mut stdin, /*id*/ 2, "first", "A").await;

    let second = read_control_frame(&mut control_reader).await;
    assert_eq!(
        second,
        json!({
            "v": 1,
            "id": 3,
            "method": "requestUserInput",
            "params": {
                "questions": [{
                    "id": "second",
                    "header": "Second",
                    "question": "Second?",
                }],
            },
        })
    );
    write_response(&mut stdin, /*id*/ 3, "second", "user_note: details").await;

    let third = read_control_frame(&mut control_reader).await;
    assert_eq!(
        third,
        json!({
            "v": 1,
            "id": 4,
            "method": "requestUserInput",
            "params": {
                "questions": [{
                    "id": "third",
                    "header": "Third",
                    "question": "Third?",
                }],
            },
        })
    );
    write_response(&mut stdin, /*id*/ 4, "third", "user_note: more").await;

    for id in 4..=64 {
        let request_id = id + 1;
        let request = read_control_frame(&mut control_reader).await;
        assert_eq!(
            request,
            json!({
                "v": 1,
                "id": request_id,
                "method": "requestUserInput",
                "params": {
                    "questions": [{
                        "id": format!("q_{id}"),
                        "header": "More",
                        "question": format!("Question {id}?"),
                    }],
                },
            })
        );
        write_response(
            &mut stdin,
            request_id,
            &format!("q_{id}"),
            "user_note: boundary",
        )
        .await;
    }

    let output_validation = read_control_frame(&mut control_reader).await;
    assert_eq!(output_validation["method"], "validateOutput");
    assert_eq!(output_validation["id"], 66);
    write_null_response(&mut stdin, /*id*/ 66).await;

    let completion = read_control_frame(&mut control_reader).await;
    assert_eq!(completion["method"], "complete");
    let markdown = completion["params"]["markdown"]
        .as_str()
        .expect("completion markdown");
    assert!(markdown.len() <= WORKFLOW_OUTPUT_MAX_BYTES);
    assert!(markdown.ends_with(&format!(
        "[Workflow output truncated to {WORKFLOW_OUTPUT_MAX_BYTES} bytes.]"
    )));
    let (json, _) = markdown
        .split_once('\u{fffd}')
        .expect("lone surrogate should normalize to the replacement character");
    assert_eq!(
        serde_json::from_str::<Value>(json).expect("workflow output should be JSON"),
        json!({
            "first": { "answers": { "first": { "answers": ["A"] } } },
            "second": { "answers": { "second": { "answers": ["user_note: details"] } } },
            "oversizedError": "Workflow input request exceeded 16384 bytes.",
            "third": { "answers": { "third": { "answers": ["user_note: more"] } } },
            "requestLimitError": "Workflow exceeded 64 user input requests.",
        })
    );
    stdin
        .write_all(b"{\"v\":1,\"id\":0,\"result\":null}\n")
        .await
        .expect("acknowledge completion");
    drop(stdin);
    assert!(
        tokio::time::timeout(RUNNER_TIMEOUT, child.wait())
            .await
            .expect("timed out waiting for Bun")
            .expect("wait for Bun")
            .success()
    );

    std::fs::write(
        source_dir.join("workflow.ts"),
        r#"export interface WorkflowInput { [key: string]: unknown }
export interface WorkflowOutput { ok: boolean }
export const inputSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  additionalProperties: true,
};
export const outputSchema = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  type: "object",
  properties: { ok: { type: "boolean" } },
  required: ["ok"],
  additionalProperties: false,
};
export default {
  apiVersion: 1,
  id: "runtime-test",
  title: "Runtime Test",
  callableName: "runtime-test",
  inputSchema,
  outputSchema,
  async run() { return { ok: true }; },
  format(_output, options) {
    if (options.format !== "markdown.v1") throw new Error("unexpected format");
    return { markdown: "output without a trailing newline" };
  },
};
"#,
    )
    .expect("replace workflow source");
    let control_dir = tempfile::tempdir().expect("create control directory");
    let control_path = control_dir.path().join("control.jsonl");
    let control_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&control_path)
        .expect("create control file");
    let control_reader = control_file.try_clone().expect("clone control file");
    let prepared_runner =
        PreparedRunner::new(RunnerOperation::Run, Some("{}"), /*expected*/ None)
            .expect("prepare workflow runner");
    let mut child = Command::new(&bun)
        .current_dir(temp_dir.path())
        .args(prepared_runner.arguments())
        .env("CODEX_WORKFLOW_CONTROL_PATH", &control_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start Bun workflow runner");
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut control_reader = WorkflowControlReader::new(TokioFile::from_std(control_reader));
    acknowledge_contract(&mut control_reader, &mut stdin).await;
    let output_validation = read_control_frame(&mut control_reader).await;
    assert_eq!(output_validation["method"], "validateOutput");
    assert_eq!(output_validation["params"]["output"], json!({ "ok": true }));
    assert_eq!(output_validation["id"], 2);
    write_null_response(&mut stdin, /*id*/ 2).await;
    let completion = read_control_frame(&mut control_reader).await;
    assert_eq!(
        completion["params"]["markdown"],
        "output without a trailing newline\n"
    );
    stdin
        .write_all(b"{\"v\":1,\"id\":0,\"result\":null}\n")
        .await
        .expect("acknowledge completion");
    drop(stdin);
    assert!(
        tokio::time::timeout(RUNNER_TIMEOUT, child.wait())
            .await
            .expect("timed out waiting for Bun")
            .expect("wait for Bun")
            .success()
    );
}

#[test]
fn workflow_user_input_call_ids_are_unique_across_turns() {
    assert_eq!(
        workflow_user_input_call_id("turn-a", /*request_id*/ 1),
        "workflow-user-input-turn-a-1"
    );
    assert_ne!(
        workflow_user_input_call_id("turn-a", /*request_id*/ 1),
        workflow_user_input_call_id("turn-b", /*request_id*/ 1)
    );
}

#[test]
fn workflow_input_uses_the_selected_environment_path_convention() {
    let (cwd, expected) = if cfg!(windows) {
        (
            PathUri::parse("file:///srv/remote%20project").expect("POSIX cwd URI"),
            "/srv/remote project",
        )
    } else {
        (
            PathUri::parse("file:///C:/remote%20project").expect("Windows cwd URI"),
            r"C:\remote project",
        )
    };
    let input = normalize_workflow_input_with_working_directory(
        &cwd.inferred_native_path_string(),
        json!({}),
        /*flags*/ serde_json::Map::new(),
    )
    .expect("normalize workflow input");

    assert_eq!(input, json!({ "workingDirectory": expected }));
}

#[test]
fn workflow_control_response_is_byte_bounded_and_preserves_unicode() {
    let unicode = json!({ "answer": "é".repeat(1_024) });
    let response = WorkflowControlResponse {
        v: WORKFLOW_CONTROL_VERSION,
        id: 1,
        result: Some(unicode),
        error: None,
    };
    let encoded = encode_control_response(&response).expect("unicode response should encode");
    assert_eq!(
        serde_json::from_slice::<WorkflowControlResponse>(&encoded)
            .expect("encoded response should be JSON"),
        response
    );

    let oversized = WorkflowControlResponse {
        v: WORKFLOW_CONTROL_VERSION,
        id: 1,
        result: Some(json!({
            "answer": "x".repeat(MAX_WORKFLOW_CONTROL_RESPONSE_BYTES),
        })),
        error: None,
    };
    assert_eq!(
        encode_control_response(&oversized)
            .expect_err("oversized response should be rejected")
            .to_string(),
        format!("workflow control response exceeded {MAX_WORKFLOW_CONTROL_RESPONSE_BYTES} bytes")
    );
}

#[test]
fn hosted_progress_frames_are_accepted_without_becoming_input_requests() {
    let line = BoundedLine {
        bytes: format!(
            "{WORKFLOW_CONTROL_PREFIX}{}",
            json!({
                "v": WORKFLOW_CONTROL_VERSION,
                "id": 0,
                "method": "progress",
                "params": {
                    "message": "Loading repository",
                    "data": { "step": 1 },
                },
            })
        )
        .into_bytes(),
        oversized: false,
    };

    assert!(matches!(
        decode_workflow_control_event(&line),
        Ok(WorkflowControlEvent::Progress)
    ));
}

#[test]
fn hosted_completion_frames_enforce_frame_and_markdown_bounds() {
    let oversized_markdown = BoundedLine {
        bytes: format!(
            "{WORKFLOW_CONTROL_PREFIX}{}",
            json!({
                "v": WORKFLOW_CONTROL_VERSION,
                "id": 0,
                "method": "complete",
                "params": { "markdown": "x".repeat(WORKFLOW_OUTPUT_MAX_BYTES + 1) },
            })
        )
        .into_bytes(),
        oversized: false,
    };
    let Err(error) = decode_workflow_control_event(&oversized_markdown) else {
        panic!("oversized markdown must be rejected");
    };
    assert_eq!(
        error,
        format!("workflow markdown exceeded {WORKFLOW_OUTPUT_MAX_BYTES} bytes")
    );

    let oversized_frame = BoundedLine {
        bytes: format!(
            "{WORKFLOW_CONTROL_PREFIX}{}",
            json!({
                "v": WORKFLOW_CONTROL_VERSION,
                "id": 0,
                "method": "complete",
                "params": {
                    "markdown": "x".repeat(MAX_WORKFLOW_COMPLETION_FRAME_BYTES),
                },
            })
        )
        .into_bytes(),
        oversized: false,
    };
    let Err(error) = decode_workflow_control_event(&oversized_frame) else {
        panic!("oversized completion frame must be rejected");
    };
    assert_eq!(
        error,
        format!("workflow completion frame exceeded {MAX_WORKFLOW_COMPLETION_FRAME_BYTES} bytes")
    );
}

#[test]
fn workflow_failure_details_prefer_stderr_and_fall_back_to_stdout() {
    assert_eq!(
        workflow_failure_details(b"stderr details\n", b"stdout details\n"),
        "stderr details"
    );
    assert_eq!(
        workflow_failure_details(b" \n", b"stdout details\n"),
        "stdout details"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn windows_process_guard_terminates_descendants() {
    let temp_dir = tempfile::tempdir().expect("create process test directory");
    let child_pid_path = temp_dir.path().join("child.pid");
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$child = Start-Process powershell.exe -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 300' -PassThru; Set-Content -LiteralPath $env:CODEX_WORKFLOW_TEST_CHILD_PID -Value $child.Id; Start-Sleep -Seconds 300",
        ])
        .env("CODEX_WORKFLOW_TEST_CHILD_PID", &child_pid_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    suspend_windows_process(&mut command);
    let mut child = command.spawn().expect("spawn suspended workflow process");
    let process_guard =
        WorkflowProcessGroupGuard::new(&child).expect("assign workflow process to job");
    resume_windows_process(&child).expect("resume workflow process");

    let descendant_pid = tokio::time::timeout(Duration::from_secs(/*secs*/ 5), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(&child_pid_path)
                && let Ok(pid) = pid.trim().parse::<u32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await
    .expect("workflow descendant should start");
    assert!(windows_process_is_running(descendant_pid));

    process_guard.terminate();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 5), child.wait())
        .await
        .expect("workflow parent should terminate")
        .expect("wait for workflow parent");
    for _ in 0..250 {
        if !windows_process_is_running(descendant_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
    }
    panic!("workflow descendant {descendant_pid} survived Job Object termination");
}

#[cfg(windows)]
fn windows_process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::STILL_ACTIVE;
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    use windows_sys::Win32::System::Threading::OpenProcess;
    use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            /*bInheritHandle*/ 0,
            pid,
        )
    };
    if handle == 0 {
        return false;
    }
    let mut exit_code = 0_u32;
    let read_exit_code = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(handle);
    }
    read_exit_code && exit_code == STILL_ACTIVE as u32
}

async fn read_control_frame(reader: &mut WorkflowControlReader) -> Value {
    let line = tokio::time::timeout(RUNNER_TIMEOUT, reader.next_line(/*max_bytes*/ 512 * 1024))
        .await
        .expect("timed out waiting for workflow control frame")
        .expect("read control frame");
    let line = String::from_utf8(line.bytes).expect("control frame should be UTF-8");
    serde_json::from_str(
        line.strip_prefix(WORKFLOW_CONTROL_PREFIX)
            .expect("control frame prefix"),
    )
    .expect("valid control frame")
}

async fn acknowledge_contract(
    reader: &mut WorkflowControlReader,
    stdin: &mut tokio::process::ChildStdin,
) {
    let contract = read_control_frame(reader).await;
    assert_eq!(contract["method"], "contract");
    assert_eq!(contract["id"], 1);
    assert!(contract["params"]["inputSchema"].is_object());
    assert!(contract["params"]["outputSchema"].is_object());
    write_null_response(stdin, /*id*/ 1).await;
}

async fn write_response(
    stdin: &mut tokio::process::ChildStdin,
    id: u64,
    question_id: &str,
    answer: &str,
) {
    let response = json!({
        "v": 1,
        "id": id,
        "result": { "answers": { (question_id): { "answers": [answer] } } },
    });
    stdin
        .write_all(format!("{response}\n").as_bytes())
        .await
        .expect("write control response");
}

async fn write_null_response(stdin: &mut tokio::process::ChildStdin, id: u64) {
    stdin
        .write_all(format!("{{\"v\":1,\"id\":{id},\"result\":null}}\n").as_bytes())
        .await
        .expect("write control response");
}
