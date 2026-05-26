use codex_workflows::WorkflowCommandCompletionSuggestion;
use codex_workflows::WorkflowCommandOptionHint;
use codex_workflows::WorkflowSummary;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkflowCommandInfo {
    pub(crate) workflow: WorkflowSummary,
    pub(crate) option_hints: Vec<WorkflowCommandOptionHint>,
    pub(crate) dynamic_suggestions: Vec<WorkflowCommandCompletionSuggestion>,
    pub(crate) completion_error: Option<String>,
    pub(crate) completion_pending: bool,
}

pub(crate) fn load_workflow_command_info(workflow: &WorkflowSummary) -> WorkflowCommandInfo {
    WorkflowCommandInfo {
        workflow: workflow.clone(),
        option_hints: workflow.command_option_hints.clone(),
        dynamic_suggestions: Vec::new(),
        completion_error: None,
        completion_pending: false,
    }
}
