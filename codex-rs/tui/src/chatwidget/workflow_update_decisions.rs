use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdateStatus;

use super::*;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::workflow_actions::WorkflowAction;
use crate::workflow_remote::WorkflowManagementData;

const VIEW_ID: &str = "workflow-update-decisions";
const CHOICE_VIEW_ID: &str = "workflow-update-choice";

impl ChatWidget {
    pub(crate) fn begin_workflow_update_decisions(&mut self) -> Option<(uuid::Uuid, PathBuf)> {
        if !self.config.features.enabled(Feature::Workflows) {
            return None;
        }
        let request_id = uuid::Uuid::new_v4();
        self.workflow_decision_request_id = Some(request_id);
        Some((request_id, self.workflow_invocation_cwd()))
    }

    pub(crate) fn reset_workflow_update_decisions(&mut self) {
        self.workflow_decision_request_id = None;
        self.workflow_prompt_seen_generation = None;
        self.bottom_pane.dismiss_view_by_id(CHOICE_VIEW_ID);
        self.bottom_pane.dismiss_view_by_id(VIEW_ID);
    }

    pub(super) fn show_workflow_update_decisions(&mut self, data: WorkflowManagementData) {
        if data.scanning || data.generation == self.workflow_prompt_seen_generation {
            return;
        }
        self.workflow_prompt_seen_generation = data.generation;
        let cwd = self.workflow_invocation_cwd();
        let items: Vec<SelectionItem> = data
            .updates
            .into_iter()
            .filter_map(|update| {
                if update.status != WorkflowUpdateStatus::Available || update.dismissed {
                    return None;
                }
                let release = update.release?;
                let managed = data.managed.iter().find(|record| record.id == update.id)?;
                if managed.managed.policy != WorkflowUpdatePolicy::Prompt
                    || self.workflow_prompt_later.contains(&(
                        update.id.clone(),
                        release.tag.clone(),
                        release.version.clone(),
                        release.commit.clone(),
                    ))
                {
                    return None;
                }
                let id = update.id;
                let installed = managed.managed.installed.clone();
                let title = release.version.as_deref().unwrap_or(&release.commit);
                let name = format!("{id} · {title}");
                let cwd = cwd.clone();
                Some(SelectionItem {
                    name,
                    description: Some("New managed release".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::OpenWorkflowUpdateDecision {
                            id: id.clone(),
                            installed: installed.clone(),
                            available: release.clone(),
                            cwd: cwd.clone(),
                        });
                    })],
                    ..Default::default()
                })
            })
            .collect();
        if items.is_empty() {
            self.bottom_pane.dismiss_view_by_id(CHOICE_VIEW_ID);
            self.bottom_pane.dismiss_view_by_id(VIEW_ID);
            return;
        }
        self.bottom_pane.dismiss_view_by_id(CHOICE_VIEW_ID);
        self.bottom_pane.dismiss_view_by_id(VIEW_ID);
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(VIEW_ID),
            title: Some("Workflow updates".to_string()),
            subtitle: Some("Select a release to decide · app-server host".to_string()),
            items,
            is_searchable: true,
            ..SelectionViewParams::picker()
        });
        self.request_redraw();
    }

    pub(crate) fn open_workflow_update_decision(
        &mut self,
        id: String,
        installed: WorkflowReleaseIdentity,
        available: WorkflowReleaseIdentity,
        cwd: PathBuf,
    ) {
        if cwd != self.workflow_invocation_cwd() {
            return;
        }
        let update_id = id.clone();
        let dismiss_id = id.clone();
        let later_id = id.clone();
        let update_installed = installed.clone();
        let update_release = available.clone();
        let dismiss_release = available.clone();
        let later_release = available.clone();
        let update_cwd = cwd.clone();
        let dismiss_cwd = cwd;
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(CHOICE_VIEW_ID),
            title: Some(format!("Update {id}?")),
            subtitle: Some(
                available
                    .version
                    .as_deref()
                    .unwrap_or(&available.commit)
                    .to_string(),
            ),
            items: vec![
                SelectionItem {
                    name: "Update".into(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::WorkflowManagementAction {
                            action: WorkflowAction::Update {
                                id: update_id.clone(),
                                installed: update_installed.clone(),
                                available: update_release.clone(),
                            },
                            cwd: update_cwd.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Dismiss this release".into(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::WorkflowManagementAction {
                            action: WorkflowAction::Dismiss {
                                id: dismiss_id.clone(),
                                installed: installed.clone(),
                                release: dismiss_release.clone(),
                            },
                            cwd: dismiss_cwd.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Later".into(),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::WorkflowUpdateLater {
                            id: later_id.clone(),
                            release: later_release.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::confirmation()
        });
        self.request_redraw();
    }

    pub(crate) fn later_workflow_update(&mut self, id: String, release: WorkflowReleaseIdentity) {
        self.workflow_prompt_later
            .insert((id, release.tag, release.version, release.commit));
        self.workflow_prompt_seen_generation = None;
        self.bottom_pane.dismiss_view_by_id(CHOICE_VIEW_ID);
        self.bottom_pane.dismiss_view_by_id(VIEW_ID);
    }
}
