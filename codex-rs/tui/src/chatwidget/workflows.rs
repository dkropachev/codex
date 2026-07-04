use super::*;
use crate::workflow_commands::WorkflowCommand;
use crate::workflow_commands::discover_workflow_commands;

impl ChatWidget {
    pub(crate) fn sync_workflow_commands(&mut self) {
        let workflows_enabled = self.config.features.enabled(Feature::Workflows);
        self.workflow_commands = if workflows_enabled {
            discover_workflow_commands(&self.config.codex_home, &self.config.cwd)
        } else {
            Vec::new()
        };
        self.bottom_pane
            .set_workflow_commands_enabled(workflows_enabled);
        self.bottom_pane
            .set_workflow_commands(self.workflow_commands.clone());
    }

    pub(super) fn current_workflow_commands(&self) -> Vec<WorkflowCommand> {
        if self.config.features.enabled(Feature::Workflows) {
            self.workflow_commands.clone()
        } else {
            Vec::new()
        }
    }

    pub(super) fn workflow_invocation_cwd(&self) -> PathBuf {
        self.current_cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.to_path_buf())
    }
}
