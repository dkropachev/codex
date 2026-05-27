use super::*;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_workflows::WORKFLOW_RUNTIME_EVENT_PREFIX;
use codex_workflows::WorkflowRuntimeEvent;
use tokio::io::AsyncReadExt;

const WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV: &str =
    "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";
const WORKFLOW_OUTPUT_FORMAT_ENV: &str = "CODEX_WORKFLOW_OUTPUT_FORMAT";
const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV: &str = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";
const WORKFLOW_COMPLETION_DEBOUNCE: Duration = Duration::from_millis(150);
const WORKFLOW_COMPLETION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub(crate) struct WorkflowRunState {
    pub(crate) workflow_name: String,
    pub(crate) markdown_result_emitted: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedWorkflowMarkdownHandoff {
    pub(crate) destination_thread_id: Option<ThreadId>,
    pub(crate) markdown: String,
}

impl App {
    pub(crate) fn start_workflow_command_completion_request(
        &mut self,
        workflow: codex_workflows::WorkflowSummary,
        input: codex_workflows::WorkflowCommandInput,
    ) {
        let command = workflow
            .command
            .clone()
            .unwrap_or_else(|| workflow.id.clone());
        self.next_workflow_completion_request_id =
            self.next_workflow_completion_request_id.wrapping_add(1);
        let request_id = self.next_workflow_completion_request_id;
        if let Some((_request_id, task)) = self.pending_workflow_completion_tasks.remove(&command) {
            task.abort();
        }

        let working_directory = self.chat_widget.config_ref().cwd.clone();
        let tx = self.app_event_tx.clone();
        let command_for_event = command.clone();
        let input_for_event = input.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(WORKFLOW_COMPLETION_DEBOUNCE).await;
            let entrypoint = workflow.path.join("src/workflow.ts");
            let result = match tokio::time::timeout(
                WORKFLOW_COMPLETION_TIMEOUT,
                codex_workflows::complete_workflow(
                    &workflow.path,
                    working_directory.as_path(),
                    &entrypoint,
                    &input,
                ),
            )
            .await
            {
                Ok(Ok(suggestions)) => Ok(suggestions),
                Ok(Err(err)) => Err(format!("{err:#}")),
                Err(_) => Err("completion timed out".to_string()),
            };
            tx.send(AppEvent::WorkflowCommandCompletionResult {
                request_id,
                command: command_for_event,
                input: input_for_event,
                result,
            });
        });
        self.pending_workflow_completion_tasks
            .insert(command, (request_id, task));
    }

    pub(crate) fn handle_workflow_command_completion_result(
        &mut self,
        request_id: u64,
        command: String,
        input: codex_workflows::WorkflowCommandInput,
        result: std::result::Result<
            Vec<codex_workflows::WorkflowCommandCompletionSuggestion>,
            String,
        >,
    ) {
        if self
            .pending_workflow_completion_tasks
            .get(&command)
            .is_none_or(|(pending_request_id, _task)| *pending_request_id != request_id)
        {
            return;
        }
        self.pending_workflow_completion_tasks.remove(&command);
        self.chat_widget
            .apply_workflow_command_completion_result(command, input, result);
    }

    pub(crate) fn run_workflow_command(&mut self, command: Vec<String>) {
        if command.is_empty() {
            self.chat_widget
                .add_error_message("Usage: /workflow <command>".to_string());
            return;
        }

        let display_command = shlex::try_join(command.iter().map(String::as_str))
            .unwrap_or_else(|_| command.join(" "));
        let workflow_name = if command.first().is_some_and(|value| value == "workflow") {
            let target_index = match command.get(1).map(String::as_str) {
                Some("fix" | "repair" | "show" | "validate" | "where") => 2,
                Some(_) => 1,
                None => 0,
            };
            command
                .get(target_index)
                .cloned()
                .unwrap_or_else(|| display_command.clone())
        } else {
            command
                .first()
                .cloned()
                .unwrap_or_else(|| display_command.clone())
        };
        let origin_thread_id = self.current_displayed_thread_id();
        let origin_thread_id_for_events = origin_thread_id
            .as_ref()
            .map(std::string::ToString::to_string);
        let run_id = Uuid::new_v4().to_string();

        let cwd = self.config.cwd.clone();
        let app_event_tx = self.app_event_tx.clone();
        let executable = self
            .config
            .codex_self_exe
            .clone()
            .unwrap_or_else(|| "codex".into());
        let mut child_command = Command::new(executable);
        let child_args = if command.first().is_some_and(|value| value == "workflow") {
            command.clone()
        } else {
            std::iter::once("workflow".to_string())
                .chain(command.iter().cloned())
                .collect()
        };
        child_command.args(&child_args);
        child_command
            .current_dir(cwd)
            .env(WORKFLOW_RUN_ID_ENV, &run_id)
            .env(WORKFLOW_APPROVALS_ENV, "delegate")
            .env(WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV, "defer")
            .env(WORKFLOW_OUTPUT_FORMAT_ENV, "tui.markdown.v1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(thread_id) = origin_thread_id.as_ref() {
            child_command.env(WORKFLOW_ORIGIN_THREAD_ID_ENV, thread_id.to_string());
        }

        match child_command.spawn() {
            Ok(mut child) => {
                self.workflow_runs.insert(
                    run_id.clone(),
                    WorkflowRunState {
                        workflow_name: workflow_name.clone(),
                        markdown_result_emitted: false,
                    },
                );
                self.chat_widget.show_workflow_process_status(
                    format!("Workflow {workflow_name}: starting"),
                    /*details*/ None,
                );
                tokio::spawn(async move {
                    let stdout_task = child.stdout.take().map(|stdout| {
                        tokio::spawn(async move {
                            let mut reader = BufReader::new(stdout);
                            let mut output = String::new();
                            match reader.read_to_string(&mut output).await {
                                Ok(_) => output,
                                Err(err) => format!("failed to read workflow stdout: {err}"),
                            }
                        })
                    });
                    let stderr_task = child.stderr.take().map(|stderr| {
                        let app_event_tx = app_event_tx.clone();
                        let run_id = run_id.clone();
                        let origin_thread_id_for_events = origin_thread_id_for_events.clone();
                        tokio::spawn(async move {
                            read_workflow_child_stderr(
                                stderr,
                                app_event_tx,
                                run_id,
                                origin_thread_id_for_events,
                            )
                            .await
                        })
                    });

                    let wait_result = child.wait().await;

                    let stdout = match stdout_task {
                        Some(task) => match task.await {
                            Ok(output) => output,
                            Err(err) => format!("failed to join workflow stdout task: {err}"),
                        },
                        None => String::new(),
                    };
                    let stderr_output = match stderr_task {
                        Some(task) => match task.await {
                            Ok(output) => output,
                            Err(err) => format!("failed to join workflow stderr task: {err}"),
                        },
                        None => String::new(),
                    };

                    let result = match wait_result {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => {
                            let stderr_output = stderr_output.trim();
                            if stderr_output.is_empty() {
                                Err(format!("workflow exited with {status}"))
                            } else {
                                Err(format!("workflow exited with {status}\n{stderr_output}"))
                            }
                        }
                        Err(err) => Err(format!("failed to wait for workflow process: {err}")),
                    };
                    app_event_tx.send(AppEvent::WorkflowProcessFinished {
                        run_id,
                        command,
                        stdout,
                        result,
                    });
                });
            }
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to start workflow: {err}"));
            }
        }
    }

    pub(crate) fn handle_workflow_process_finished(
        &mut self,
        run_id: String,
        command: Vec<String>,
        stdout: String,
        result: Result<(), String>,
    ) {
        let workflow_state = self.workflow_runs.remove(&run_id);
        let display_command = shlex::try_join(command.iter().map(String::as_str))
            .unwrap_or_else(|_| command.join(" "));
        if let Err(err) = result {
            let markdown_result_was_emitted = workflow_state
                .as_ref()
                .is_some_and(|state| state.markdown_result_emitted);
            if !(markdown_result_was_emitted
                && err.contains("workflow host closed the connection without returning a result"))
            {
                self.chat_widget
                    .add_error_message(format!("Workflow failed: {display_command}: {err}"));
            }
        } else if let Some(state) = workflow_state
            && !state.markdown_result_emitted
        {
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(json) => {
                    self.chat_widget
                        .add_to_history(crate::history_cell::WorkflowJsonCell::new(json));
                }
                Err(_) if command.first().is_some_and(|arg| arg == "workflow") => {
                    self.chat_widget.add_to_history(
                        crate::history_cell::WorkflowMarkdownCell::new(
                            stdout.trim_end().to_string(),
                            self.config.cwd.as_path(),
                        ),
                    );
                }
                Err(err) => {
                    self.chat_widget.add_error_message(format!(
                        "Workflow result for {display_command} was not valid JSON: {err}"
                    ));
                }
            }
        }

        if self.workflow_runs.is_empty() {
            self.chat_widget.hide_workflow_process_status();
        }
    }

    pub(crate) fn handle_workflow_progress_notification(
        &mut self,
        notification: WorkflowProgressNotification,
    ) {
        let workflow_name = self
            .workflow_runs
            .get(&notification.run_id)
            .map(|state| state.workflow_name.as_str());
        self.chat_widget.handle_workflow_progress_notification(
            workflow_name,
            notification,
            /*replay_kind*/ None,
        );
    }

    pub(crate) fn handle_workflow_markdown_result_notification(
        &mut self,
        notification: WorkflowMarkdownResultNotification,
    ) {
        if let Some(state) = self.workflow_runs.get_mut(&notification.run_id) {
            state.markdown_result_emitted = true;
        }
        let destination_thread_id = notification
            .thread_id
            .as_deref()
            .and_then(|thread_id| ThreadId::from_string(thread_id).ok());
        self.queue_workflow_markdown_handoff(destination_thread_id, notification.markdown.clone());
        self.chat_widget
            .handle_workflow_markdown_result_notification(notification, None);
    }

    pub(crate) fn queue_workflow_markdown_handoff(
        &mut self,
        destination_thread_id: Option<ThreadId>,
        markdown: String,
    ) {
        self.pending_workflow_markdown_handoffs
            .push_back(QueuedWorkflowMarkdownHandoff {
                destination_thread_id,
                markdown,
            });
    }

    pub(crate) fn take_pending_workflow_markdown_handoffs_for_thread(
        &mut self,
        thread_id: ThreadId,
    ) -> Vec<QueuedWorkflowMarkdownHandoff> {
        let mut remaining = VecDeque::new();
        let mut pending = Vec::new();

        for handoff in self.pending_workflow_markdown_handoffs.drain(..) {
            if handoff.destination_thread_id.is_none()
                || handoff.destination_thread_id == Some(thread_id)
            {
                pending.push(handoff);
            } else {
                remaining.push_back(handoff);
            }
        }

        self.pending_workflow_markdown_handoffs = remaining;
        pending
    }
}

async fn read_workflow_child_stderr(
    stderr: impl tokio::io::AsyncRead + Unpin,
    app_event_tx: AppEventSender,
    run_id: String,
    origin_thread_id: Option<String>,
) -> String {
    let mut reader = BufReader::new(stderr).lines();
    let mut raw_stderr = String::new();

    loop {
        let line = match reader.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                push_stderr_line(
                    &mut raw_stderr,
                    format!("failed to read workflow stderr: {err}"),
                );
                break;
            }
        };

        if let Some(payload) = line.strip_prefix(WORKFLOW_RUNTIME_EVENT_PREFIX) {
            match serde_json::from_str::<WorkflowRuntimeEvent>(payload) {
                Ok(WorkflowRuntimeEvent::Progress { message, data }) => {
                    app_event_tx.send(AppEvent::WorkflowProgress {
                        notification: WorkflowProgressNotification {
                            run_id: run_id.clone(),
                            thread_id: origin_thread_id.clone(),
                            message,
                            data,
                            status: None,
                        },
                    });
                }
                Ok(WorkflowRuntimeEvent::Status { status }) => {
                    app_event_tx.send(AppEvent::WorkflowProgress {
                        notification: WorkflowProgressNotification {
                            run_id: run_id.clone(),
                            thread_id: origin_thread_id.clone(),
                            message: String::new(),
                            data: None,
                            status: Some(codex_app_server_protocol::WorkflowStatusUpdate {
                                workflow_name: status.workflow_name,
                                workflow_status: status.workflow_status,
                                threads: status
                                    .threads
                                    .into_iter()
                                    .map(|thread| codex_app_server_protocol::WorkflowThreadStatus {
                                        name: thread.name,
                                        status: thread.status,
                                    })
                                    .collect(),
                                child_statuses: status
                                    .child_statuses
                                    .into_iter()
                                    .map(|child| codex_app_server_protocol::WorkflowChildStatus {
                                        workflow_name: child.workflow_name,
                                        workflow_status: child.workflow_status,
                                        threads: child
                                            .threads
                                            .into_iter()
                                            .map(|thread| {
                                                codex_app_server_protocol::WorkflowThreadStatus {
                                                    name: thread.name,
                                                    status: thread.status,
                                                }
                                            })
                                            .collect(),
                                    })
                                    .collect(),
                            }),
                        },
                    });
                }
                Ok(WorkflowRuntimeEvent::ReportToUserMarkdown { markdown }) => {
                    app_event_tx.send(AppEvent::WorkflowMarkdownResult {
                        notification: WorkflowMarkdownResultNotification {
                            run_id: run_id.clone(),
                            thread_id: origin_thread_id.clone(),
                            markdown,
                        },
                    });
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

    raw_stderr
}

fn push_stderr_line(stderr: &mut String, line: impl AsRef<str>) {
    stderr.push_str(line.as_ref());
    stderr.push('\n');
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_test_app;
    use super::super::tests::make_test_app_with_channels;
    use super::WorkflowRunState;
    use crate::app_event::AppEvent;
    use codex_protocol::ThreadId;
    use ratatui::text::Line;

    fn lines_to_single_string(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn workflow_command_reports_usage_without_arguments() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        while app_event_rx.try_recv().is_ok() {}

        app.run_workflow_command(Vec::new());

        let cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .find_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => Some(cell),
                _ => None,
            })
            .expect("workflow usage error should add a history cell");
        let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            insta::assert_snapshot!("workflow_command_usage_error", rendered);
        });
    }

    #[tokio::test]
    async fn workflow_markdown_handoffs_preserve_completion_order_across_threads() {
        let mut app = make_test_app().await;
        let thread_a = ThreadId::new();
        let thread_b = ThreadId::new();

        app.queue_workflow_markdown_handoff(Some(thread_a), "a-1".to_string());
        app.queue_workflow_markdown_handoff(
            /*destination_thread_id*/ None,
            "global-1".to_string(),
        );
        app.queue_workflow_markdown_handoff(Some(thread_b), "b-1".to_string());
        app.queue_workflow_markdown_handoff(Some(thread_a), "a-2".to_string());
        app.queue_workflow_markdown_handoff(
            /*destination_thread_id*/ None,
            "global-2".to_string(),
        );

        let injected_for_a = app
            .take_pending_workflow_markdown_handoffs_for_thread(thread_a)
            .into_iter()
            .map(|handoff| handoff.markdown)
            .collect::<Vec<_>>();

        assert_eq!(injected_for_a, vec!["a-1", "global-1", "a-2", "global-2"]);

        let remaining = app
            .pending_workflow_markdown_handoffs
            .iter()
            .map(|handoff| handoff.markdown.as_str())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec!["b-1"]);

        let injected_for_b = app
            .take_pending_workflow_markdown_handoffs_for_thread(thread_b)
            .into_iter()
            .map(|handoff| handoff.markdown)
            .collect::<Vec<_>>();

        assert_eq!(injected_for_b, vec!["b-1"]);
        assert!(app.pending_workflow_markdown_handoffs.is_empty());
    }

    #[tokio::test]
    async fn workflow_process_finish_suppresses_host_closed_error_after_markdown_handoff() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        while app_event_rx.try_recv().is_ok() {}

        let run_id = "run-1".to_string();
        app.workflow_runs.insert(
            run_id.clone(),
            WorkflowRunState {
                workflow_name: "code-review".to_string(),
                markdown_result_emitted: true,
            },
        );

        app.handle_workflow_process_finished(
            run_id,
            vec!["code-review".to_string()],
            String::new(),
            Err(
                "workflow exited with exit status: 1\nError: failed to run workflow code-review\nCaused by:\n    workflow host closed the connection without returning a result"
                    .to_string(),
            ),
        );

        assert!(app.workflow_runs.is_empty());
        assert!(
            std::iter::from_fn(|| app_event_rx.try_recv().ok())
                .all(|event| { !matches!(event, AppEvent::InsertHistoryCell(_)) })
        );
    }
}
