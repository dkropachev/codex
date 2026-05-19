use super::*;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV: &str =
    "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";
const WORKFLOW_APP_SERVER_URL_ENV: &str = "CODEX_WORKFLOW_APP_SERVER_URL";
const CODEX_APP_SERVER_URL_ENV: &str = "CODEX_APP_SERVER_URL";
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
        let run_id = Uuid::new_v4().to_string();

        let Some(app_server_url) = self.workflow_app_server_url.clone() else {
            self.chat_widget.add_error_message(
                "No workflow app-server is available. Enable `[features].workflows = true` and restart regular Codex."
                    .to_string(),
            );
            return;
        };

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
            .env(CODEX_APP_SERVER_URL_ENV, &app_server_url)
            .env(WORKFLOW_APP_SERVER_URL_ENV, &app_server_url)
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
                self.chat_widget.add_info_message(
                    format!("Workflow started: {display_command}"),
                    Some(format!("Connected to {app_server_url}")),
                );
                tokio::spawn(async move {
                    let stderr_task = child.stderr.take().map(|mut stderr| {
                        tokio::spawn(async move {
                            let mut output = String::new();
                            let read_result = stderr.read_to_string(&mut output).await;
                            (output, read_result)
                        })
                    });

                    let result = match child.wait().await {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => {
                            let stderr_output = match stderr_task {
                                Some(task) => match task.await {
                                    Ok((output, Ok(_))) => output,
                                    Ok((output, Err(err))) => {
                                        format!("failed to read workflow stderr: {err}\n{output}")
                                    }
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

#[cfg(test)]
mod tests {
    use super::super::test_support::make_test_app;
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
    async fn workflow_command_reports_disabled_without_managed_app_server() {
        let mut app = make_test_app().await;

        app.run_workflow_command(vec!["node".to_string(), "workflow.js".to_string()]);

        let rendered = app
            .chat_widget
            .active_cell_transcript_lines(/*width*/ 80)
            .map(|lines| lines_to_single_string(&lines))
            .unwrap_or_default();
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            insta::assert_snapshot!("workflow_command_disabled", rendered);
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
