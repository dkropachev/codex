//! Typed `/workflow` command dispatch against the app-server host.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowCheckUpdatesParams;
use codex_app_server_protocol::WorkflowCheckUpdatesResponse;
use codex_app_server_protocol::WorkflowInstallParams;
use codex_app_server_protocol::WorkflowInstallResponse;
use codex_app_server_protocol::WorkflowManagedListParams;
use codex_app_server_protocol::WorkflowManagedListResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowResolveSourceParams;
use codex_app_server_protocol::WorkflowResolveSourceResponse;
use codex_app_server_protocol::WorkflowUpdateEntry;
use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdateStatus;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use uuid::Uuid;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::workflow_actions::WorkflowAction;

fn policy(value: &str) -> anyhow::Result<WorkflowUpdatePolicy> {
    match value {
        "prompt" => Ok(WorkflowUpdatePolicy::Prompt),
        "automatic" => Ok(WorkflowUpdatePolicy::Automatic),
        "manual" => Ok(WorkflowUpdatePolicy::Manual),
        _ => anyhow::bail!("policy must be prompt, automatic, or manual"),
    }
}

fn install_arguments(args: &str) -> anyhow::Result<(String, WorkflowUpdatePolicy)> {
    let raw = args.trim().trim_start_matches("install").trim();
    if raw.is_empty() {
        anyhow::bail!("usage: /workflow install <source> [--policy prompt|automatic|manual]");
    }
    let (raw_source, policy) = match raw.rsplit_once(" --policy ") {
        Some((source, value)) if !value.contains(char::is_whitespace) => {
            (source.trim(), policy(value)?)
        }
        _ => (raw, WorkflowUpdatePolicy::Prompt),
    };
    let source = raw_source
        .strip_prefix('"')
        .and_then(|source| source.strip_suffix('"'))
        .or_else(|| {
            raw_source
                .strip_prefix('\'')
                .and_then(|source| source.strip_suffix('\''))
        })
        .unwrap_or(raw_source);
    if source.is_empty() {
        anyhow::bail!("workflow source is required");
    }
    Ok((source.to_string(), policy))
}

async fn available_release(
    handle: &AppServerRequestHandle,
    id: &str,
    nonce: Uuid,
) -> anyhow::Result<WorkflowReleaseIdentity> {
    let mut cursor = None;
    for page in 0..42 {
        let response: WorkflowUpdatesReadResponse = handle
            .request_typed(ClientRequest::WorkflowUpdatesRead {
                request_id: RequestId::String(format!("workflow-slash-updates-{nonce}-{page}")),
                params: WorkflowUpdatesReadParams {
                    cursor,
                    limit: Some(100),
                },
            })
            .await?;
        if let Some(WorkflowUpdateEntry {
            status: WorkflowUpdateStatus::Available,
            release: Some(release),
            ..
        }) = response.data.into_iter().find(|entry| entry.id == id)
        {
            return Ok(release);
        }
        match response.next_cursor {
            Some(next_cursor) => cursor = Some(next_cursor),
            None => anyhow::bail!("no available release for {id}; run /workflow check-updates"),
        }
    }
    anyhow::bail!("workflow update state exceeded its page limit")
}

async fn installed_release(
    handle: &AppServerRequestHandle,
    id: &str,
    nonce: Uuid,
) -> anyhow::Result<WorkflowReleaseIdentity> {
    let mut cursor = None;
    for page in 0..42 {
        let response: WorkflowManagedListResponse = handle
            .request_typed(ClientRequest::WorkflowManagedList {
                request_id: RequestId::String(format!("workflow-slash-managed-{nonce}-{page}")),
                params: WorkflowManagedListParams {
                    cursor,
                    limit: Some(100),
                },
            })
            .await?;
        if let Some(record) = response.data.into_iter().find(|record| record.id == id) {
            return Ok(record.managed.installed);
        }
        match response.next_cursor {
            Some(next_cursor) => cursor = Some(next_cursor),
            None => anyhow::bail!("managed workflow {id} is not installed"),
        }
    }
    anyhow::bail!("managed workflow list exceeded its page limit")
}

pub(crate) fn spawn_command(
    handle: AppServerRequestHandle,
    args: String,
    cwd: PathBuf,
    app_event_tx: AppEventSender,
) {
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
            let words = if args.split_whitespace().next() == Some("install") {
                vec![
                    "install".to_string(),
                    args.trim().trim_start_matches("install").trim().to_string(),
                ]
            } else {
                shlex::split(&args).context("invalid workflow command quoting")?
            };
            let nonce = Uuid::new_v4();
            let Some((command, rest)) = words.split_first() else {
                anyhow::bail!(
                    "usage: /workflow <install|check-updates|update|set-policy|uninstall>"
                );
            };
            match command.as_str() {
                "install" => {
                    let (source, policy) = install_arguments(&args)?;
                    let resolved: WorkflowResolveSourceResponse = handle
                        .request_typed(ClientRequest::WorkflowResolveSource {
                            request_id: RequestId::String(format!("workflow-resolve-{nonce}")),
                            params: WorkflowResolveSourceParams {
                                source: source.clone(),
                                cwd: Some(cwd.to_string_lossy().into_owned()),
                            },
                        })
                        .await?;
                    let installed: WorkflowInstallResponse = handle
                        .request_typed(ClientRequest::WorkflowInstall {
                            request_id: RequestId::String(format!("workflow-install-{nonce}")),
                            params: WorkflowInstallParams {
                                source,
                                cwd: Some(cwd.to_string_lossy().into_owned()),
                                expected_release: resolved.release,
                                policy,
                            },
                        })
                        .await?;
                    app_event_tx.send(AppEvent::WorkflowManagementActionFinished {
                        cwd: cwd.clone(),
                        result: Ok(format!(
                            "Installed workflow {} on the app-server host",
                            installed.id
                        )),
                    });
                }
                "check-updates" if rest.is_empty() => {
                    let response: WorkflowCheckUpdatesResponse = handle
                        .request_typed(ClientRequest::WorkflowCheckUpdates {
                            request_id: RequestId::String(format!("workflow-check-{nonce}")),
                            params: WorkflowCheckUpdatesParams {},
                        })
                        .await?;
                    app_event_tx.send(AppEvent::WorkflowManagementActionFinished {
                        cwd: cwd.clone(),
                        result: Ok(if response.started {
                            format!("Workflow update scan {} started", response.scan_id)
                        } else {
                            format!(
                                "Workflow update scan {} is already running",
                                response.scan_id
                            )
                        }),
                    });
                }
                "update" | "set-policy" | "uninstall" | "dismiss" => {
                    let id = rest.first().context("workflow ID is required")?;
                    let installed = installed_release(&handle, id, nonce).await?;
                    match command.as_str() {
                        "update" if rest.len() == 1 => {
                            let available = available_release(&handle, id, nonce).await?;
                            app_event_tx.send(AppEvent::WorkflowManagementAction {
                                action: WorkflowAction::Update {
                                    id: id.clone(),
                                    installed,
                                    available,
                                },
                                cwd: cwd.clone(),
                            });
                        }
                        "set-policy" if rest.len() == 2 => {
                            app_event_tx.send(AppEvent::WorkflowManagementAction {
                                action: WorkflowAction::SetPolicy {
                                    id: id.clone(),
                                    installed,
                                    policy: policy(&rest[1])?,
                                },
                                cwd: cwd.clone(),
                            });
                        }
                        "uninstall" if rest.len() == 1 => {
                            app_event_tx.send(AppEvent::ConfirmWorkflowUninstall {
                                id: id.clone(),
                                installed,
                                cwd: cwd.clone(),
                            });
                        }
                        "dismiss" if rest.len() == 1 => {
                            let release = available_release(&handle, id, nonce).await?;
                            app_event_tx.send(AppEvent::WorkflowManagementAction {
                                action: WorkflowAction::Dismiss {
                                    id: id.clone(),
                                    installed,
                                    release,
                                },
                                cwd: cwd.clone(),
                            });
                        }
                        _ => anyhow::bail!("usage: /workflow {command} <id> [policy]"),
                    }
                }
                _ => anyhow::bail!("unknown workflow command: {command}"),
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| "workflow command timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()));
        if let Err(error) = result {
            app_event_tx.send(AppEvent::WorkflowManagementActionFinished {
                cwd,
                result: Err(error),
            });
        }
    });
}

#[cfg(test)]
#[path = "workflow_slash_tests.rs"]
mod tests;
