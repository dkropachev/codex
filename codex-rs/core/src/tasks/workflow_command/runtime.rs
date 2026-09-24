use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_protocol::request_user_input::RequestUserInputResponse;
use codex_workflows::MAX_WORKFLOW_USER_INPUT_REQUESTS;
use codex_workflows::WorkflowContract;
use codex_workflows::WorkflowPackage;
use codex_workflows::decode_user_input_request;
use codex_workflows::normalize_workflow_input_with_working_directory;
use codex_workflows::parse_completion;
use codex_workflows::parse_control_request;
use codex_workflows::runner::MAX_RUNNER_INPUT_BYTES;
use codex_workflows::runner::MAX_WORKFLOW_COMPLETION_FRAME_BYTES;
use codex_workflows::runner::MAX_WORKFLOW_CONTROL_FRAME_BYTES;
use codex_workflows::runner::MAX_WORKFLOW_RUN_FRAME_BYTES;
use codex_workflows::runner::PreparedRunner;
use codex_workflows::runner::RunnerOperation;
use codex_workflows::runner::WORKFLOW_CONTROL_PREFIX;
use codex_workflows::runner::WORKFLOW_CONTROL_VERSION;
use codex_workflows::runner::WorkflowControlResponse;
use codex_workflows::runner::WorkflowProgressParams;
use codex_workflows::runner::decode_control_frame;
use codex_workflows::runner::encode_control_response;
use codex_workflows::validate_user_input_response;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::truncate_error_output;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

mod host;

use host::BoundedDiagnostics;
use host::BoundedLine;
use host::WorkflowControlReader;
use host::WorkflowProcessGroupGuard;
use host::resume_windows_process;
use host::suspend_windows_process;

const WORKFLOW_EXIT_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 2);

enum WorkflowControlEvent<'a> {
    Completion(String),
    Contract {
        id: u64,
        input_schema: Value,
        output_schema: Value,
    },
    OutputValidation {
        id: u64,
        output: Value,
    },
    Progress,
    Request(&'a str),
}

pub(super) async fn run_workflow_for_tui(
    workflow_dir: &Path,
    input: &Value,
    session: Arc<Session>,
    turn_context: Arc<TurnContext>,
    cancellation_token: &CancellationToken,
) -> Result<Option<String>, String> {
    let package_path = workflow_dir.to_path_buf();
    let package =
        tokio::task::spawn_blocking(move || WorkflowPackage::load_executable(&package_path))
            .await
            .map_err(|err| format!("workflow package validation task failed: {err}"))?
            .map_err(|err| {
                format!(
                    "failed to load workflow package at {}: {err:#}",
                    workflow_dir.display()
                )
            })?;
    let input_has_working_directory = input
        .as_object()
        .is_some_and(|input| input.contains_key("workingDirectory"));
    let working_directory = if input_has_working_directory {
        String::new()
    } else {
        let workflow_environment = turn_context
            .environments
            .resolve_primary()
            .await
            .map_err(|err| format!("workflow primary environment failed to start: {err}"))?
            .ok_or_else(|| "workflow command requires a ready primary environment".to_string())?;
        workflow_environment.cwd().inferred_native_path_string()
    };
    let input = normalize_workflow_input_with_working_directory(
        &working_directory,
        input.clone(),
        /*flags*/ serde_json::Map::new(),
    )
    .map_err(|err| err.message().to_string())?;
    let input_json = serde_json::to_string(&input)
        .map_err(|err| format!("failed to serialize workflow input: {err}"))?;
    if input_json.len() > MAX_RUNNER_INPUT_BYTES {
        return Err(format!(
            "workflow input exceeded {MAX_RUNNER_INPUT_BYTES} bytes"
        ));
    }
    let prepared_runner = PreparedRunner::new(
        RunnerOperation::Run,
        Some(&input_json),
        Some(&package.manifest),
    )
    .map_err(|err| format!("failed to prepare workflow runner: {err:#}"))?;

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
        .args(prepared_runner.arguments())
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
    let mut user_input_request_count = 0_u64;
    let mut contract = None::<WorkflowContract>;
    let mut output_validated = false;

    loop {
        let line = tokio::select! {
            () = cancellation_token.cancelled() => return Ok(None),
            line = control_reader.next_line(
                MAX_WORKFLOW_RUN_FRAME_BYTES + WORKFLOW_CONTROL_PREFIX.len(),
            ) => line.map_err(|err| format!("failed to read workflow control channel: {err}"))?,
            status = &mut child_wait => {
                process_guard.terminate();
                let stderr = stderr_diagnostics.finish().await;
                let stdout = stdout_diagnostics.finish().await;
                return Err(workflow_exit_error(status, &stderr, &stdout));
            }
        };
        match decode_workflow_control_event(&line)? {
            WorkflowControlEvent::Completion(markdown) => {
                if contract.is_none() || !output_validated {
                    return Err(
                        "workflow completed before contract and output validation".to_string()
                    );
                }
                if cancellation_token.is_cancelled() {
                    return Ok(None);
                }
                write_control_response(
                    &mut child_stdin,
                    &WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id: 0,
                        result: Some(Value::Null),
                        error: None,
                    },
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
            WorkflowControlEvent::Contract {
                id,
                input_schema,
                output_schema,
            } => {
                if id != expected_request_id {
                    return Err(format!(
                        "workflow control request id {id} was out of order; expected {expected_request_id}"
                    ));
                }
                expected_request_id = expected_request_id.saturating_add(1);
                let built = WorkflowContract::from_schemas(input_schema, output_schema)
                    .map_err(|err| format!("{err:#}"))
                    .and_then(|candidate| {
                        candidate.validate_input(&input)?;
                        Ok(candidate)
                    });
                let response = match built {
                    Ok(candidate) if contract.is_none() => {
                        contract = Some(candidate);
                        WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id,
                            result: Some(Value::Null),
                            error: None,
                        }
                    }
                    Ok(_) => WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id,
                        result: None,
                        error: Some("workflow contract was already initialized".to_string()),
                    },
                    Err(error) => WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id,
                        result: None,
                        error: Some(error),
                    },
                };
                write_control_response(&mut child_stdin, &response).await?;
            }
            WorkflowControlEvent::OutputValidation { id, output } => {
                if id != expected_request_id {
                    return Err(format!(
                        "workflow control request id {id} was out of order; expected {expected_request_id}"
                    ));
                }
                expected_request_id = expected_request_id.saturating_add(1);
                let response = match contract.as_ref() {
                    Some(contract) => match contract.validate_output(&output) {
                        Ok(()) => {
                            output_validated = true;
                            WorkflowControlResponse {
                                v: WORKFLOW_CONTROL_VERSION,
                                id,
                                result: Some(Value::Null),
                                error: None,
                            }
                        }
                        Err(error) => WorkflowControlResponse {
                            v: WORKFLOW_CONTROL_VERSION,
                            id,
                            result: None,
                            error: Some(error),
                        },
                    },
                    None => WorkflowControlResponse {
                        v: WORKFLOW_CONTROL_VERSION,
                        id,
                        result: None,
                        error: Some("workflow output arrived before its contract".to_string()),
                    },
                };
                write_control_response(&mut child_stdin, &response).await?;
            }
            WorkflowControlEvent::Progress => continue,
            WorkflowControlEvent::Request(payload) => {
                if contract.is_none() {
                    return Err("workflow requested user input before its contract".to_string());
                }
                if user_input_request_count >= MAX_WORKFLOW_USER_INPUT_REQUESTS {
                    return Err(format!(
                        "workflow exceeded the limit of {MAX_WORKFLOW_USER_INPUT_REQUESTS} user input requests"
                    ));
                }
                let request = parse_control_request(payload, expected_request_id)?;
                expected_request_id = expected_request_id.saturating_add(1);
                user_input_request_count = user_input_request_count.saturating_add(1);

                let args = decode_user_input_request(request.params);
                let args = match args {
                    Ok(args) => args,
                    Err(error) => {
                        write_control_response(
                            &mut child_stdin,
                            &WorkflowControlResponse {
                                v: WORKFLOW_CONTROL_VERSION,
                                id: request.id,
                                result: None,
                                error: Some(error),
                            },
                        )
                        .await?;
                        continue;
                    }
                };

                let call_id = workflow_user_input_call_id(&turn_context.sub_id, request.id);
                let response_contract = args.clone();
                let response = session.request_user_input(turn_context.as_ref(), call_id, args);
                tokio::pin!(response);
                let response = loop {
                    tokio::select! {
                        response = &mut response => break response,
                        status = &mut child_wait => {
                            process_guard.terminate();
                            let stderr = stderr_diagnostics.finish().await;
                            let stdout = stdout_diagnostics.finish().await;
                            return Err(workflow_exited_while_waiting(status, &stderr, &stdout));
                        }
                        line = control_reader.next_line(
                            MAX_WORKFLOW_RUN_FRAME_BYTES + WORKFLOW_CONTROL_PREFIX.len(),
                        ) => {
                            let line = line.map_err(|err| {
                                format!("failed to read workflow control channel: {err}")
                            })?;
                            if matches!(
                                decode_workflow_control_event(&line)?,
                                WorkflowControlEvent::Progress
                            ) {
                                continue;
                            }
                            return Err(
                                "workflow emitted a concurrent user input control request"
                                    .to_string(),
                            );
                        }
                        () = cancellation_token.cancelled() => return Ok(None),
                    }
                };
                let Some(response) = response else {
                    return Ok(None);
                };
                match validate_user_input_response(&response_contract, response) {
                    Ok(response) => {
                        write_user_input_response(&mut child_stdin, request.id, &response).await?;
                    }
                    Err(error) => {
                        write_control_response(
                            &mut child_stdin,
                            &WorkflowControlResponse {
                                v: WORKFLOW_CONTROL_VERSION,
                                id: request.id,
                                result: None,
                                error: Some(error),
                            },
                        )
                        .await?;
                    }
                }
            }
        }
    }
}

fn decode_workflow_control_event(line: &BoundedLine) -> Result<WorkflowControlEvent<'_>, String> {
    if line.oversized {
        return Err(format!(
            "workflow control frame exceeded {MAX_WORKFLOW_RUN_FRAME_BYTES} bytes"
        ));
    }
    let encoded = std::str::from_utf8(&line.bytes)
        .map_err(|err| format!("workflow control frame was not valid UTF-8: {err}"))?;
    let frame = decode_control_frame(encoded).map_err(|err| format!("{err:#}"))?;
    let payload = encoded
        .strip_prefix(WORKFLOW_CONTROL_PREFIX)
        .ok_or_else(|| "workflow control frame did not have the expected prefix".to_string())?;
    match frame.method.as_str() {
        "complete" => {
            if payload.len() > MAX_WORKFLOW_COMPLETION_FRAME_BYTES {
                return Err(format!(
                    "workflow completion frame exceeded {MAX_WORKFLOW_COMPLETION_FRAME_BYTES} bytes"
                ));
            }
            parse_completion(payload).map(WorkflowControlEvent::Completion)
        }
        "contract" => {
            if frame.id == 0 {
                return Err("invalid workflow contract frame header".to_string());
            }
            let input_schema = frame
                .params
                .get("inputSchema")
                .cloned()
                .ok_or_else(|| "workflow contract frame omitted inputSchema".to_string())?;
            let output_schema = frame
                .params
                .get("outputSchema")
                .cloned()
                .ok_or_else(|| "workflow contract frame omitted outputSchema".to_string())?;
            Ok(WorkflowControlEvent::Contract {
                id: frame.id,
                input_schema,
                output_schema,
            })
        }
        "validateOutput" => {
            if frame.id == 0 {
                return Err("invalid workflow output validation frame header".to_string());
            }
            let output = frame
                .params
                .get("output")
                .cloned()
                .ok_or_else(|| "workflow output validation frame omitted output".to_string())?;
            Ok(WorkflowControlEvent::OutputValidation {
                id: frame.id,
                output,
            })
        }
        "progress" => {
            if payload.len() > MAX_WORKFLOW_CONTROL_FRAME_BYTES {
                return Err(format!(
                    "workflow control frame exceeded {MAX_WORKFLOW_CONTROL_FRAME_BYTES} bytes"
                ));
            }
            if frame.id != 0 {
                return Err("invalid workflow progress frame header".to_string());
            }
            let progress = serde_json::from_value::<WorkflowProgressParams>(frame.params)
                .map_err(|err| format!("invalid workflow progress frame: {err}"))?;
            if progress.message.trim().is_empty() {
                return Err("workflow progress message must not be empty".to_string());
            }
            Ok(WorkflowControlEvent::Progress)
        }
        _ => {
            if payload.len() > MAX_WORKFLOW_CONTROL_FRAME_BYTES {
                return Err(format!(
                    "workflow control frame exceeded {MAX_WORKFLOW_CONTROL_FRAME_BYTES} bytes"
                ));
            }
            Ok(WorkflowControlEvent::Request(payload))
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
    write_control_response(
        child_stdin,
        &WorkflowControlResponse {
            v: WORKFLOW_CONTROL_VERSION,
            id: request_id,
            result: Some(json!({ "answers": answers })),
            error: None,
        },
    )
    .await
}

async fn write_control_response(
    child_stdin: &mut ChildStdin,
    response: &WorkflowControlResponse,
) -> Result<(), String> {
    let encoded = encode_control_response(response).map_err(|err| format!("{err:#}"))?;
    child_stdin
        .write_all(&encoded)
        .await
        .map_err(|err| format!("failed to write workflow control response: {err}"))?;
    child_stdin
        .flush()
        .await
        .map_err(|err| format!("failed to flush workflow control response: {err}"))
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
