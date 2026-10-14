//! Workflow discovery from the app-server host for local and remote TUI sessions.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowSummary;
use codex_app_server_protocol::WorkflowUpdateEntry;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use uuid::Uuid;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::workflow_commands::WorkflowCommand;

const PAGE_SIZE: u32 = 100;
const MAX_COMMANDS: usize = 4_096;
const MAX_PAGES: usize = 42;

#[derive(Debug)]
pub(crate) struct WorkflowManagementData {
    pub(crate) workflows: Vec<WorkflowSummary>,
    pub(crate) updates: Vec<WorkflowUpdateEntry>,
    pub(crate) generation: Option<u64>,
    pub(crate) scanning: bool,
    pub(crate) error: Option<String>,
}

async fn fetch_workflows(
    handle: &AppServerRequestHandle,
    cwd: &Path,
    nonce: Uuid,
) -> anyhow::Result<Vec<WorkflowSummary>> {
    let mut workflows = Vec::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor = None;
    for page in 0..MAX_PAGES {
        let response: WorkflowListResponse = handle
            .request_typed(ClientRequest::WorkflowList {
                request_id: RequestId::String(format!("workflow-list-{nonce}-{page}")),
                params: WorkflowListParams {
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    cursor,
                    limit: Some(PAGE_SIZE),
                },
            })
            .await?;
        if workflows.len() + response.data.len() > MAX_COMMANDS {
            anyhow::bail!("workflow discovery exceeded its result limit");
        }
        workflows.extend(response.data);
        match response.next_cursor {
            Some(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                cursor = Some(next_cursor);
            }
            Some(_) => anyhow::bail!("workflow discovery returned a repeated cursor"),
            None => return Ok(workflows),
        }
    }
    anyhow::bail!("workflow discovery exceeded its page limit")
}

pub(crate) fn spawn_discovery(
    handle: AppServerRequestHandle,
    generation: u64,
    cwd: PathBuf,
    app_event_tx: AppEventSender,
) {
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(/*secs*/ 15),
            fetch_workflows(&handle, &cwd, Uuid::new_v4()),
        )
        .await
        .map_err(|_| "workflow discovery timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map(|workflows| {
            workflows
                .into_iter()
                .map(|entry| WorkflowCommand {
                    id: entry.id,
                    command: entry.command,
                    description: entry.description,
                    option_hints: Vec::new(),
                    workflow_dir: PathBuf::from(entry.path),
                })
                .collect()
        });
        app_event_tx.send(AppEvent::WorkflowCommandsLoaded {
            generation,
            cwd,
            result,
        });
    });
}

pub(crate) fn spawn_management(
    handle: AppServerRequestHandle,
    request_id: Uuid,
    cwd: PathBuf,
    app_event_tx: AppEventSender,
) {
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), async {
            let workflows = fetch_workflows(&handle, &cwd, request_id).await?;
            let mut updates = Vec::new();
            let mut generation = None;
            let mut scanning = false;
            let mut error = None;
            let mut cursor = None;
            let mut seen_cursors = HashSet::new();
            let mut complete = false;
            for page in 0..MAX_PAGES {
                let response: WorkflowUpdatesReadResponse = match handle
                    .request_typed(ClientRequest::WorkflowUpdatesRead {
                        request_id: RequestId::String(format!(
                            "workflow-updates-{request_id}-{page}"
                        )),
                        params: WorkflowUpdatesReadParams {
                            cursor,
                            limit: Some(PAGE_SIZE),
                        },
                    })
                    .await
                {
                    Ok(response) => response,
                    Err(read_error) => {
                        error = Some(read_error.to_string());
                        complete = true;
                        break;
                    }
                };
                if generation.is_some_and(|previous| previous != response.generation) {
                    anyhow::bail!("workflow update snapshot changed during pagination");
                }
                generation = Some(response.generation);
                scanning = response.scanning;
                error = response.error;
                if updates.len() + response.data.len() > MAX_COMMANDS {
                    anyhow::bail!("workflow update snapshot exceeded its result limit");
                }
                updates.extend(response.data);
                match response.next_cursor {
                    Some(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                        cursor = Some(next_cursor);
                    }
                    Some(_) => anyhow::bail!("workflow updates returned a repeated cursor"),
                    None => {
                        complete = true;
                        break;
                    }
                }
            }
            if !complete {
                anyhow::bail!("workflow updates exceeded its page limit");
            }
            Ok::<_, anyhow::Error>(WorkflowManagementData {
                workflows,
                updates,
                generation,
                scanning,
                error,
            })
        })
        .await
        .map_err(|_| "workflow management request timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string()));
        app_event_tx.send(AppEvent::WorkflowManagementLoaded {
            request_id,
            cwd,
            result,
        });
    });
}
