use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ManagedWorkflowInfo;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowSummary;
use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_core::config::Config;
use codex_features::Feature;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowRecord;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::discover_workflow_commands_bounded;

use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::error_code::method_not_found;
use crate::outgoing_message::OutgoingMessageSender;

#[path = "workflows/update_state.rs"]
mod update_state;
use update_state::WorkflowUpdates;

const DEFAULT_PAGE_LIMIT: usize = 50;
const MAX_PAGE_LIMIT: usize = 100;
const MAX_CWD_BYTES: usize = 4_096;
const MAX_CURSOR_BYTES: usize = 512;

pub(crate) struct WorkflowListProcessor {
    config: Arc<Config>,
    updates: Option<WorkflowUpdates>,
}

impl WorkflowListProcessor {
    pub(crate) fn new(config: Arc<Config>, outgoing: Arc<OutgoingMessageSender>) -> Self {
        let updates = config
            .features
            .enabled(Feature::Workflows)
            .then(|| WorkflowUpdates::start(Arc::clone(&config), outgoing));
        Self { config, updates }
    }

    pub(crate) async fn updates_read(
        &self,
        params: WorkflowUpdatesReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let Some(updates) = &self.updates else {
            return Err(method_not_found(
                "workflow management requires the workflows feature",
            ));
        };
        updates.wait_for_recovery().await;
        Ok(Some(updates.read(params).await?.into()))
    }

    pub(crate) async fn list(
        &self,
        params: WorkflowListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        if !self.config.features.enabled(Feature::Workflows) {
            return Err(method_not_found(
                "workflow management requires the workflows feature",
            ));
        }
        if let Some(updates) = &self.updates {
            updates.wait_for_recovery().await;
        }
        let limit = params
            .limit
            .map(|limit| limit as usize)
            .unwrap_or(DEFAULT_PAGE_LIMIT);
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(invalid_params(
                "workflow list limit must be between 1 and 100",
            ));
        }
        let cwd = match params.cwd {
            Some(cwd) => {
                if cwd.len() > MAX_CWD_BYTES {
                    return Err(invalid_params("workflow cwd is too long"));
                }
                AbsolutePathBuf::from_absolute_path_checked(PathBuf::from(cwd))
                    .map_err(|error| invalid_params(format!("invalid workflow cwd: {error}")))?
            }
            None => self.config.cwd.clone(),
        };
        let offset = match params.cursor {
            Some(cursor) if cursor.len() <= MAX_CURSOR_BYTES => cursor
                .parse::<usize>()
                .ok()
                .filter(|offset| *offset <= 4_096)
                .ok_or_else(|| invalid_params("invalid workflow list cursor"))?,
            Some(_) => return Err(invalid_params("workflow list cursor is too long")),
            None => 0,
        };
        let config = Arc::clone(&self.config);
        let response =
            tokio::task::spawn_blocking(move || list(config.as_ref(), &cwd, offset, limit))
                .await
                .map_err(|error| internal_error(format!("workflow list task failed: {error}")))?
                .map_err(|error| internal_error(format!("workflow list failed: {error:#}")))?;
        Ok(Some(response.into()))
    }
}

fn list(
    config: &Config,
    cwd: &AbsolutePathBuf,
    offset: usize,
    limit: usize,
) -> anyhow::Result<WorkflowListResponse> {
    let management = config.codex_home.join(".workflow-management");
    let has_entries = |root: &AbsolutePathBuf| -> anyhow::Result<bool> {
        Ok(root.as_path().is_dir() && fs::read_dir(root.as_path())?.next().is_some())
    };
    let managed = if has_entries(&management.join("receipts"))?
        || has_entries(&management.join("journals"))?
    {
        ManagedWorkflowService::new(&config.codex_home, &config.codex_home.join("workflows"))?
            .list_installed()?
    } else {
        Vec::new()
    };
    let root = config.codex_home.join("workflows");
    let managed_by_path = managed
        .into_iter()
        .map(|record| (root.join(&record.id).to_path_buf(), record))
        .collect::<HashMap<_, _>>();
    let mut data = discover_workflow_commands_bounded(config.codex_home.as_path(), cwd.as_path())?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(|command| {
            let managed = managed_by_path
                .get(&command.workflow_dir)
                .filter(|record| record.id == command.id)
                .map(managed_info);
            WorkflowSummary {
                id: command.id,
                command: command.command,
                description: command.description.chars().take(2_048).collect(),
                path: command.workflow_dir.display().to_string(),
                managed,
            }
        })
        .collect::<Vec<_>>();
    let next_cursor = if data.len() > limit {
        data.pop();
        Some((offset + data.len()).to_string())
    } else {
        None
    };
    Ok(WorkflowListResponse { data, next_cursor })
}

fn managed_info(record: &ManagedWorkflowRecord) -> ManagedWorkflowInfo {
    ManagedWorkflowInfo {
        source: record.source.clone(),
        installed: release(&record.installed),
        policy: match record.policy {
            codex_workflows::WorkflowUpdatePolicy::Prompt => WorkflowUpdatePolicy::Prompt,
            codex_workflows::WorkflowUpdatePolicy::Automatic => WorkflowUpdatePolicy::Automatic,
            codex_workflows::WorkflowUpdatePolicy::Manual => WorkflowUpdatePolicy::Manual,
        },
        dismissed_release: record.dismissed_release.as_ref().map(release),
    }
}

fn release(release: &codex_workflows::WorkflowReleaseIdentity) -> WorkflowReleaseIdentity {
    WorkflowReleaseIdentity {
        tag: release.tag.clone(),
        version: release.version.clone(),
        commit: release.commit.clone(),
    }
}
