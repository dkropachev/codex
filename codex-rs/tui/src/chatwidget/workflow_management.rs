use std::collections::HashMap;

use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdateStatus;

use super::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::workflow_actions::WorkflowAction;
use crate::workflow_remote::WorkflowManagementData;

pub(super) const VIEW_ID: &str = "workflow-management";

impl ChatWidget {
    pub(crate) fn open_workflow_management(&mut self) {
        if !self.config.features.enabled(Feature::Workflows) {
            self.add_error_message("Workflows are disabled.".to_string());
            return;
        }
        let request_id = uuid::Uuid::new_v4();
        let cwd = self.workflow_invocation_cwd();
        self.workflow_management_request_id = Some(request_id);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(VIEW_ID),
            title: Some("Workflows".to_string()),
            subtitle: Some("Loading from the app-server host…".to_string()),
            items: vec![SelectionItem {
                name: "Loading workflow state".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            ..SelectionViewParams::picker()
        });
        self.app_event_tx
            .send(AppEvent::OpenWorkflowManagement { request_id, cwd });
        self.request_redraw();
    }

    pub(crate) fn on_workflow_management_loaded(
        &mut self,
        request_id: uuid::Uuid,
        cwd: PathBuf,
        result: Result<WorkflowManagementData, String>,
    ) {
        if self.workflow_management_request_id != Some(request_id)
            || cwd != self.workflow_invocation_cwd()
            || !self.bottom_pane.dismiss_active_view_if_id(VIEW_ID)
        {
            return;
        }
        self.workflow_management_request_id = None;
        let data = match result {
            Ok(data) => data,
            Err(error) => {
                self.add_error_message(format!("Cannot load workflows: {error}"));
                return;
            }
        };
        let updates_by_id: HashMap<_, _> = data
            .updates
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let mut items: Vec<SelectionItem> = Vec::new();
        for workflow in &data.workflows {
            let (name, policy) = match &workflow.managed {
                Some(managed) => {
                    let release = managed
                        .installed
                        .version
                        .as_deref()
                        .unwrap_or(&managed.installed.commit);
                    let policy = match managed.policy {
                        WorkflowUpdatePolicy::Prompt => "prompt",
                        WorkflowUpdatePolicy::Automatic => "automatic",
                        WorkflowUpdatePolicy::Manual => "manual",
                    };
                    (format!("{} · {release}", workflow.id), policy)
                }
                None => (format!("{} · project", workflow.id), "unmanaged"),
            };
            let status = match updates_by_id.get(workflow.id.as_str()) {
                Some(update) => match update.status {
                    WorkflowUpdateStatus::Pending => "checking".to_string(),
                    WorkflowUpdateStatus::Current => "current".to_string(),
                    WorkflowUpdateStatus::Available if update.dismissed => {
                        "available · dismissed".to_string()
                    }
                    WorkflowUpdateStatus::Available => "update available".to_string(),
                    WorkflowUpdateStatus::Error => format!(
                        "error: {}",
                        update.error.as_deref().unwrap_or("source unavailable")
                    ),
                },
                None => "not checked".to_string(),
            };
            items.push(SelectionItem {
                name,
                description: Some(format!("{} · {policy} · {status}", workflow.description)),
                is_disabled: true,
                ..Default::default()
            });
            let Some(managed) = &workflow.managed else {
                continue;
            };
            let id = workflow.id.clone();
            let installed = managed.installed.clone();
            let cwd = cwd.clone();
            let mut action_item = |name: &str, action: WorkflowAction| {
                let cwd = cwd.clone();
                items.push(SelectionItem {
                    name: name.to_string(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::WorkflowManagementAction {
                            action: action.clone(),
                            cwd: cwd.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                });
            };
            if let Some(update) = updates_by_id.get(workflow.id.as_str())
                && update.status == WorkflowUpdateStatus::Available
                && let Some(release) = &update.release
            {
                action_item(
                    "Update",
                    WorkflowAction::Update {
                        id: id.clone(),
                        installed: installed.clone(),
                        available: release.clone(),
                    },
                );
                if managed.policy == WorkflowUpdatePolicy::Prompt && !update.dismissed {
                    action_item(
                        "Dismiss this release",
                        WorkflowAction::Dismiss {
                            id: id.clone(),
                            installed: installed.clone(),
                            release: release.clone(),
                        },
                    );
                }
            }
            for (name, policy) in [
                ("Set prompt policy", WorkflowUpdatePolicy::Prompt),
                ("Set automatic policy", WorkflowUpdatePolicy::Automatic),
                ("Set manual policy", WorkflowUpdatePolicy::Manual),
            ] {
                if managed.policy != policy {
                    action_item(
                        name,
                        WorkflowAction::SetPolicy {
                            id: id.clone(),
                            installed: installed.clone(),
                            policy,
                        },
                    );
                }
            }
        }
        if items.is_empty() {
            items.push(SelectionItem {
                name: "No workflows found".to_string(),
                is_disabled: true,
                ..Default::default()
            });
        }
        let subtitle = match data.error {
            Some(error) => format!("App-server host · update check error: {error}"),
            None if data.scanning => "App-server host · checking releases".to_string(),
            None => format!(
                "App-server host · update snapshot {}",
                data.generation.unwrap_or_default()
            ),
        };
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(VIEW_ID),
            title: Some("Workflows".to_string()),
            subtitle: Some(subtitle),
            footer_hint: Some("Esc to close".into()),
            is_searchable: true,
            items,
            ..SelectionViewParams::picker()
        });
        self.request_redraw();
    }
}
