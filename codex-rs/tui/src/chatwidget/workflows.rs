use super::*;
use codex_app_server_protocol::WorkflowMarkdownResultNotification;
use codex_app_server_protocol::WorkflowProgressNotification;

impl ChatWidget {
    /// Show or refresh the workflow status row with the provided details.
    pub(crate) fn show_workflow_process_status(&mut self, header: String, details: Option<String>) {
        self.bottom_pane.ensure_status_indicator();
        if !self.bottom_pane.is_task_running() {
            self.bottom_pane
                .set_interrupt_hint_visible(/*visible*/ false);
        }
        self.set_status(
            header,
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
        workflow_name: Option<&str>,
        notification: WorkflowProgressNotification,
        replay_kind: Option<ReplayKind>,
    ) {
        if replay_kind.is_some() {
            return;
        }

        let WorkflowProgressNotification {
            message,
            data,
            status,
            ..
        } = notification;
        let (header, details) = match status {
            Some(status) => workflow_status_surface(&status),
            None => workflow_legacy_status_surface(workflow_name, &message, data.as_ref()),
        };

        self.show_workflow_process_status(header, details);
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

pub(crate) fn workflow_status_surface(
    status: &codex_app_server_protocol::WorkflowStatusUpdate,
) -> (String, Option<String>) {
    let header = format!(
        "Workflow {}: {}",
        status.workflow_name, status.workflow_status,
    );
    let details = if status.threads.len() > 1 {
        Some(
            status
                .threads
                .iter()
                .map(|thread| format!("-> {}: {}", thread.name, thread.status))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    } else {
        None
    };
    (header, details)
}

fn workflow_legacy_status_surface(
    workflow_name: Option<&str>,
    message: &str,
    data: Option<&serde_json::Value>,
) -> (String, Option<String>) {
    let status = workflow_progress_details(message, data);
    let header = match workflow_name {
        Some(workflow_name) if !workflow_name.is_empty() => {
            format!("Workflow {workflow_name}: {status}")
        }
        _ => format!("Workflow: {status}"),
    };
    (header, None)
}

fn workflow_progress_details(message: &str, data: Option<&serde_json::Value>) -> String {
    let message = message.trim();
    let summary = data.and_then(format_workflow_progress_data);
    match (message.is_empty(), summary) {
        (true, Some(summary)) => summary,
        (false, Some(summary)) => format!("{message} ({summary})"),
        (false, None) => message.to_string(),
        (true, None) => String::new(),
    }
}

fn format_workflow_progress_data(data: &serde_json::Value) -> Option<String> {
    match data {
        serde_json::Value::Null => None,
        serde_json::Value::Object(object) => {
            let mut parts = Vec::new();

            if let Some(stage) = object.get("stage").and_then(simple_workflow_value) {
                parts.push(stage);
            }

            match (
                object.get("step").and_then(simple_workflow_value),
                object.get("total").and_then(simple_workflow_value),
            ) {
                (Some(step), Some(total)) => parts.push(format!("step {step}/{total}")),
                (Some(step), None) => parts.push(format!("step {step}")),
                _ => {}
            }

            for (key, value) in object {
                if matches!(key.as_str(), "stage" | "step" | "total") {
                    continue;
                }
                if let Some(part) = named_workflow_value(key, value) {
                    parts.push(part);
                }
            }

            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        other => simple_workflow_value(other)
            .or_else(|| crate::text_formatting::format_json_compact(&other.to_string())),
    }
}

fn simple_workflow_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Array(items) => {
            let items = items
                .iter()
                .filter_map(simple_workflow_value)
                .collect::<Vec<_>>();
            if items.is_empty() {
                None
            } else {
                Some(items.join(", "))
            }
        }
        serde_json::Value::Object(_) => None,
    }
}

fn named_workflow_value(key: &str, value: &serde_json::Value) -> Option<String> {
    if let Some(value) = simple_workflow_value(value) {
        return Some(format!("{key} {value}"));
    }

    match value {
        serde_json::Value::Null => None,
        _ => crate::text_formatting::format_json_compact(&value.to_string())
            .map(|value| format!("{key}: {value}")),
    }
}
