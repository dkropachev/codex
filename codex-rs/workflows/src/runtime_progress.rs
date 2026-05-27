use crate::execute::WorkflowCommandProgress;
use crate::execute::WorkflowCommandProgressHandler;
use crate::workflow_runtime::WorkflowRuntimeEvent;
use crate::workflow_runtime::WorkflowRuntimeEventHandler;

const WORKFLOW_RUN_ID_ENV: &str = "CODEX_WORKFLOW_RUN_ID";

pub(crate) fn standalone_cli_runtime_event_handler<'a>(
    progress: Option<&'a WorkflowCommandProgressHandler<'a>>,
) -> Option<Box<WorkflowRuntimeEventHandler<'a>>> {
    if let Some(progress) = progress
        && std::env::var_os(WORKFLOW_RUN_ID_ENV).is_none()
    {
        Some(Box::new(move |event: &WorkflowRuntimeEvent| match event {
            WorkflowRuntimeEvent::Status { status } => {
                let workflow_name = &status.workflow_name;
                let workflow_status = &status.workflow_status;
                progress(WorkflowCommandProgress {
                    message: format!("Workflow {workflow_name}: {workflow_status}"),
                    data: None,
                });
                for thread in &status.threads {
                    let name = &thread.name;
                    let thread_status = &thread.status;
                    progress(WorkflowCommandProgress {
                        message: format!("  -> {name}: {thread_status}"),
                        data: None,
                    });
                }
                for child in &status.child_statuses {
                    let workflow_name = &child.workflow_name;
                    let workflow_status = &child.workflow_status;
                    progress(WorkflowCommandProgress {
                        message: format!("Child workflow {workflow_name}: {workflow_status}"),
                        data: None,
                    });
                    for thread in &child.threads {
                        let name = &thread.name;
                        let thread_status = &thread.status;
                        progress(WorkflowCommandProgress {
                            message: format!("  -> {name}: {thread_status}"),
                            data: None,
                        });
                    }
                }
            }
            WorkflowRuntimeEvent::Progress { message, data } => {
                progress(WorkflowCommandProgress {
                    message: message.clone(),
                    data: data.clone(),
                });
            }
            WorkflowRuntimeEvent::ReportToUserMarkdown { .. } => {}
        }) as Box<WorkflowRuntimeEventHandler<'a>>)
    } else {
        None
    }
}
