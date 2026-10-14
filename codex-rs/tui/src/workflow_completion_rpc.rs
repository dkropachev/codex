//! Workflow option completion on the app-server host.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowCompleteParams;
use codex_app_server_protocol::WorkflowCompleteResponse;
use codex_app_server_protocol::WorkflowCompletionMode;
use codex_workflows::CompletionItem;
use codex_workflows::CompletionMode;
use codex_workflows::CompletionRequest;
use codex_workflows::CompletionResult;
use uuid::Uuid;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

pub(crate) struct WorkflowCompletionTask {
    pub(crate) id: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) generation: u64,
    pub(crate) workflow_dir: PathBuf,
    pub(crate) request: CompletionRequest,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) tx: AppEventSender,
}

pub(crate) fn spawn_completion(handle: AppServerRequestHandle, task: WorkflowCompletionTask) {
    let WorkflowCompletionTask {
        id,
        cwd,
        generation,
        workflow_dir,
        request,
        cancelled,
        tx,
    } = task;
    std::mem::drop(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(/*millis*/ 75)).await;
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        let result = match id {
            Some(id) => {
                let response = tokio::time::timeout(
                    Duration::from_secs(/*secs*/ 30),
                    handle.request_typed::<WorkflowCompleteResponse>(
                        ClientRequest::WorkflowComplete {
                            request_id: RequestId::String(format!(
                                "workflow-complete-{}",
                                Uuid::new_v4()
                            )),
                            params: WorkflowCompleteParams {
                                id,
                                cwd: Some(cwd.to_string_lossy().into_owned()),
                                input: request.input.clone(),
                                active_field: request.active_field.clone(),
                                prefix: request.prefix.clone(),
                                mode: match request.mode {
                                    CompletionMode::Field => WorkflowCompletionMode::Field,
                                    CompletionMode::Value => WorkflowCompletionMode::Value,
                                },
                            },
                        },
                    ),
                )
                .await;
                match response {
                    Ok(Ok(response)) => CompletionResult::new(
                        response
                            .items
                            .into_iter()
                            .map(|item| CompletionItem {
                                value: item.value,
                                description: item.description,
                            })
                            .collect(),
                        response.error,
                    ),
                    Ok(Err(error)) => CompletionResult::new(Vec::new(), Some(error.to_string())),
                    Err(_) => CompletionResult::new(
                        Vec::new(),
                        Some("Workflow completion timed out".to_string()),
                    ),
                }
            }
            None => CompletionResult::new(
                Vec::new(),
                Some("Workflow command is no longer available".to_string()),
            ),
        };
        if !cancelled.load(Ordering::Relaxed) {
            tx.send(AppEvent::WorkflowCompletionResult {
                generation,
                workflow_dir,
                request,
                result,
            });
        }
    }));
}

#[cfg(test)]
#[path = "workflow_completion_rpc_tests.rs"]
mod tests;
