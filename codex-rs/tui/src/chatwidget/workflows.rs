use super::workflow_management::VIEW_ID as WORKFLOW_MANAGEMENT_VIEW_ID;
use super::*;
use crate::workflow_commands::WorkflowCommand;

impl ChatWidget {
    pub(crate) fn sync_workflow_commands(&mut self) {
        self.workflow_management_request_id = None;
        self.bottom_pane
            .dismiss_view_by_id(WORKFLOW_MANAGEMENT_VIEW_ID);
        let workflows_enabled = self.config.features.enabled(Feature::Workflows);
        self.workflow_commands_generation = self.workflow_commands_generation.wrapping_add(1);
        self.workflow_commands.clear();
        let completion_cwd = self.workflow_invocation_cwd();
        self.bottom_pane.set_workflow_commands_context(
            workflows_enabled,
            completion_cwd.clone(),
            self.workflow_commands.clone(),
        );
        if workflows_enabled {
            self.app_event_tx.send(AppEvent::RefreshWorkflowCommands {
                generation: self.workflow_commands_generation,
                cwd: completion_cwd,
            });
            self.app_event_tx
                .send(AppEvent::RefreshWorkflowUpdateDecisions);
        }
    }

    pub(crate) fn apply_workflow_commands_result(
        &mut self,
        generation: u64,
        cwd: PathBuf,
        result: Result<Vec<WorkflowCommand>, String>,
    ) {
        if generation != self.workflow_commands_generation
            || cwd != self.workflow_invocation_cwd()
            || !self.config.features.enabled(Feature::Workflows)
        {
            return;
        }
        match result {
            Ok(commands) => self.workflow_commands = commands,
            Err(error) => self.add_error_message(format!("Workflow discovery failed: {error}")),
        }
        self.bottom_pane.set_workflow_commands_context(
            /*enabled*/ true,
            cwd,
            self.workflow_commands.clone(),
        );
        self.request_redraw();
    }

    pub(super) fn current_workflow_commands(&self) -> Vec<WorkflowCommand> {
        if self.config.features.enabled(Feature::Workflows) {
            self.workflow_commands.clone()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn workflow_id_for_dir(&self, workflow_dir: &std::path::Path) -> Option<String> {
        self.workflow_commands
            .iter()
            .find(|command| command.workflow_dir == workflow_dir)
            .map(|command| command.id.clone())
    }

    pub(crate) fn workflow_invocation_cwd(&self) -> PathBuf {
        self.current_cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.to_path_buf())
    }
}
