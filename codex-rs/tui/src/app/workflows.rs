use super::*;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;
use codex_workflows::WORKFLOW_RUNTIME_EVENT_PREFIX;
use codex_workflows::WorkflowRuntimeEvent;

const WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV: &str =
    "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";
const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV: &str = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";

#[derive(Debug, Clone)]
pub(crate) struct WorkflowRunState {
    pub(crate) origin_thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedWorkflowMarkdownHandoff {
    pub(crate) destination_thread_id: Option<ThreadId>,
    pub(crate) markdown: String,
}

impl App {
    pub(crate) fn run_workflow_command(&mut self, command: Vec<String>) {
        if command.is_empty() {
            self.chat_widget
                .add_error_message("Usage: /workflow <command>".to_string());
            return;
        }

        let display_command = shlex::try_join(command.iter().map(String::as_str))
            .unwrap_or_else(|_| command.join(" "));
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
        child_command.args(&command);
        child_command
            .current_dir(cwd)
            .env(WORKFLOW_RUN_ID_ENV, &run_id)
            .env(WORKFLOW_APPROVALS_ENV, "delegate")
            .env(WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV, "defer")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(thread_id) = origin_thread_id.as_ref() {
            child_command.env(WORKFLOW_ORIGIN_THREAD_ID_ENV, thread_id.to_string());
        }

        match child_command.spawn() {
            Ok(mut child) => {
                self.workflow_runs
                    .insert(run_id.clone(), WorkflowRunState { origin_thread_id });
                self.chat_widget
                    .show_workflow_process_status(Some(display_command.clone()));
                self.chat_widget
                    .add_info_message(format!("Workflow started: {display_command}"), None);
                tokio::spawn(async move {
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

                    let result = match child.wait().await {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => {
                            let stderr_output = match stderr_task {
                                Some(task) => match task.await {
                                    Ok(output) => output,
                                    Err(err) => {
                                        format!("failed to join workflow stderr task: {err}")
                                    }
                                },
                                None => String::new(),
                            };
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
        result: Result<(), String>,
    ) {
        let _origin_thread_id = self
            .workflow_runs
            .remove(&run_id)
            .map(|state| state.origin_thread_id);
        let display_command = shlex::try_join(command.iter().map(String::as_str))
            .unwrap_or_else(|_| command.join(" "));
        match result {
            Ok(()) => self.chat_widget.add_info_message(
                format!("Workflow finished: {display_command}"),
                /*hint*/ None,
            ),
            Err(err) => self
                .chat_widget
                .add_error_message(format!("Workflow failed: {display_command}: {err}")),
        }

        if self.workflow_runs.is_empty() {
            self.chat_widget.hide_workflow_process_status();
        }
    }

    pub(crate) fn handle_workflow_progress_notification(
        &mut self,
        notification: WorkflowProgressNotification,
    ) {
        self.chat_widget
            .handle_workflow_progress_notification(notification, None);
    }

    pub(crate) fn handle_workflow_markdown_result_notification(
        &mut self,
        notification: WorkflowMarkdownResultNotification,
    ) {
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
        app.queue_workflow_markdown_handoff(None, "global-1".to_string());
        app.queue_workflow_markdown_handoff(Some(thread_b), "b-1".to_string());
        app.queue_workflow_markdown_handoff(Some(thread_a), "a-2".to_string());
        app.queue_workflow_markdown_handoff(None, "global-2".to_string());

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
}
