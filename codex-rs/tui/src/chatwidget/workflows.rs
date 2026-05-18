use super::*;
use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;

impl ChatWidget {
    /// Show or refresh the workflow status row with the provided details.
    pub(crate) fn show_workflow_process_status(&mut self, details: Option<String>) {
        self.bottom_pane.ensure_status_indicator();
        if !self.bottom_pane.is_task_running() {
            self.bottom_pane
                .set_interrupt_hint_visible(/*visible*/ false);
        }
        self.set_status(
            "Workflow".to_string(),
            details,
            StatusDetailsCapitalization::Preserve,
            STATUS_DETAILS_DEFAULT_MAX_LINES,
        );
    }

    /// Hide the workflow status row if the workflow no longer has anything active to show.
    pub(crate) fn hide_workflow_process_status(&mut self) {
        self.bottom_pane.hide_status_indicator();
    }

    pub(crate) fn handle_workflow_progress_notification(
        &mut self,
        notification: WorkflowProgressNotification,
        replay_kind: Option<ReplayKind>,
    ) {
        if replay_kind.is_some() {
            return;
        }

        let WorkflowProgressNotification { message, data, .. } = notification;
        let details = match data {
            Some(data) => {
                let data = serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
                if message.is_empty() {
                    data
                } else {
                    format!("{message}\n{data}")
                }
            }
            None => message,
        };

        self.show_workflow_process_status(Some(details));
    }

    pub(crate) fn handle_workflow_markdown_result_notification(
        &mut self,
        notification: WorkflowMarkdownResultNotification,
        _replay_kind: Option<ReplayKind>,
    ) {
        let WorkflowMarkdownResultNotification {
            markdown,
            thread_id,
            ..
        } = notification;
        if thread_id.is_none() {
            return;
        }

        let cwd = self.config_ref().cwd.to_path_buf();
        self.add_to_history(crate::history_cell::WorkflowMarkdownCell::new(
            markdown, &cwd,
        ));
    }
}
