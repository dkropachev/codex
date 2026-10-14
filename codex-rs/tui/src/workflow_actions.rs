//! Exact-release workflow mutations on the app-server host.

use std::path::PathBuf;
use std::time::Duration;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowDismissParams;
use codex_app_server_protocol::WorkflowDismissResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowSetPolicyParams;
use codex_app_server_protocol::WorkflowSetPolicyResponse;
use codex_app_server_protocol::WorkflowUninstallParams;
use codex_app_server_protocol::WorkflowUninstallResponse;
use codex_app_server_protocol::WorkflowUpdateParams;
use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdateResponse;
use uuid::Uuid;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

#[derive(Clone, Debug)]
pub(crate) enum WorkflowAction {
    Update {
        id: String,
        installed: WorkflowReleaseIdentity,
        available: WorkflowReleaseIdentity,
    },
    SetPolicy {
        id: String,
        installed: WorkflowReleaseIdentity,
        policy: WorkflowUpdatePolicy,
    },
    Dismiss {
        id: String,
        installed: WorkflowReleaseIdentity,
        release: WorkflowReleaseIdentity,
    },
    Uninstall {
        id: String,
        installed: WorkflowReleaseIdentity,
    },
}

pub(crate) fn spawn_action(
    handle: AppServerRequestHandle,
    action: WorkflowAction,
    cwd: PathBuf,
    app_event_tx: AppEventSender,
) {
    tokio::spawn(async move {
        let request_id = RequestId::String(format!("workflow-action-{}", Uuid::new_v4()));
        let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 120), async {
            match action {
                WorkflowAction::Update {
                    id,
                    installed,
                    available,
                } => {
                    let response: WorkflowUpdateResponse = handle
                        .request_typed(ClientRequest::WorkflowUpdate {
                            request_id,
                            params: WorkflowUpdateParams {
                                id,
                                expected_installed: installed,
                                expected_available: available,
                            },
                        })
                        .await?;
                    Ok::<_, anyhow::Error>(format!(
                        "Installed workflow release {}",
                        response
                            .installed
                            .version
                            .unwrap_or(response.installed.commit)
                    ))
                }
                WorkflowAction::SetPolicy {
                    id,
                    installed,
                    policy,
                } => {
                    let _: WorkflowSetPolicyResponse = handle
                        .request_typed(ClientRequest::WorkflowSetPolicy {
                            request_id,
                            params: WorkflowSetPolicyParams {
                                id,
                                expected_installed: installed,
                                policy,
                            },
                        })
                        .await?;
                    Ok("Workflow policy updated".to_string())
                }
                WorkflowAction::Dismiss {
                    id,
                    installed,
                    release,
                } => {
                    let _: WorkflowDismissResponse = handle
                        .request_typed(ClientRequest::WorkflowDismiss {
                            request_id,
                            params: WorkflowDismissParams {
                                id,
                                expected_installed: installed,
                                release,
                            },
                        })
                        .await?;
                    Ok("Workflow release dismissed".to_string())
                }
                WorkflowAction::Uninstall { id, installed } => {
                    let _: WorkflowUninstallResponse = handle
                        .request_typed(ClientRequest::WorkflowUninstall {
                            request_id,
                            params: WorkflowUninstallParams {
                                id,
                                expected_installed: installed,
                            },
                        })
                        .await?;
                    Ok("Workflow uninstalled".to_string())
                }
            }
        })
        .await
        .map_err(|_| "workflow action timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()));
        app_event_tx.send(AppEvent::WorkflowManagementActionFinished { cwd, result });
    });
}
