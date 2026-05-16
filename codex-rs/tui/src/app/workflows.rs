use super::*;
use std::process::Stdio;
use tokio::process::Command;

const WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV: &str =
    "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";
const WORKFLOW_APP_SERVER_URL_ENV: &str = "CODEX_WORKFLOW_APP_SERVER_URL";
const CODEX_APP_SERVER_URL_ENV: &str = "CODEX_APP_SERVER_URL";

impl App {
    pub(crate) fn run_workflow_command(&mut self, command: Vec<String>) {
        if command.is_empty() {
            self.chat_widget
                .add_error_message("Usage: /workflow <command>".to_string());
            return;
        }

        let display_command = shlex::try_join(command.iter().map(String::as_str))
            .unwrap_or_else(|_| command.join(" "));

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
            .env(WORKFLOW_APPROVALS_ENV, "delegate")
            .env(WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV, "defer")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        match child_command.spawn() {
            Ok(mut child) => {
                self.chat_widget.add_info_message(
                    format!("Workflow started: {display_command}"),
                    Some(format!("Connected to {app_server_url}")),
                );
                tokio::spawn(async move {
                    let result = match child.wait().await {
                        Ok(status) if status.success() => Ok(()),
                        Ok(status) => Err(format!("workflow exited with {status}")),
                        Err(err) => Err(format!("failed to wait for workflow process: {err}")),
                    };
                    app_event_tx.send(AppEvent::WorkflowProcessFinished { command, result });
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
        command: Vec<String>,
        result: Result<(), String>,
    ) {
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
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_test_app;
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
}
