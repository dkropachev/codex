use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowCompleteParams;
use codex_app_server_protocol::WorkflowCompleteResponse;
use codex_app_server_protocol::WorkflowCompletionItem;
use codex_app_server_protocol::WorkflowCompletionMode;
use codex_core::config::Config;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::CompletionMode;
use codex_workflows::CompletionRequest;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::complete_published_workflow_cancellable;
use codex_workflows::complete_workflow_cancellable;
use codex_workflows::discover_workflow_commands_bounded;

use super::MAX_CWD_BYTES;
use super::OVERLOADED_ERROR_CODE;
use super::WorkflowListProcessor;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;

const MAX_ID_BYTES: usize = 240;
const MAX_INPUT_BYTES: usize = 64 * 1_024;
const MAX_PREFIX_BYTES: usize = 1_024;
const MAX_ERROR_CHARS: usize = 2_048;

impl WorkflowListProcessor {
    pub(crate) async fn complete(
        &self,
        params: WorkflowCompleteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.ready_for_mutation().await?;
        if params.id.is_empty() || params.id.len() > MAX_ID_BYTES {
            return Err(invalid_params("workflow ID is empty or too long"));
        }
        if params.prefix.len() > MAX_PREFIX_BYTES
            || params
                .active_field
                .as_ref()
                .is_some_and(|field| field.len() > MAX_PREFIX_BYTES)
        {
            return Err(invalid_params(
                "workflow completion field or prefix is too long",
            ));
        }
        let input_len = serde_json::to_vec(&params.input)
            .map_err(|error| invalid_params(format!("invalid workflow completion input: {error}")))?
            .len();
        if input_len > MAX_INPUT_BYTES {
            return Err(invalid_params("workflow completion input is too large"));
        }
        let cwd = match params.cwd.as_ref() {
            Some(cwd) if cwd.len() <= MAX_CWD_BYTES => {
                AbsolutePathBuf::from_absolute_path_checked(PathBuf::from(cwd))
                    .map_err(|error| invalid_params(format!("invalid workflow cwd: {error}")))?
            }
            Some(_) => return Err(invalid_params("workflow cwd is too long")),
            None => self.config.cwd.clone(),
        };
        let config = Arc::clone(&self.config);
        let permit = Arc::clone(&self.completion_requests)
            .try_acquire_owned()
            .map_err(|_| JSONRPCErrorError {
                code: OVERLOADED_ERROR_CODE,
                message: "workflow completion request limit reached; retry shortly".into(),
                data: None,
            })?;
        let response = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            complete(config.as_ref(), &cwd, params)
        })
        .await
        .map_err(|error| internal_error(format!("workflow completion task failed: {error}")))?
        .map_err(|error| internal_error(format!("workflow completion failed: {error:#}")))?
        .ok_or_else(|| invalid_params("workflow ID is not discoverable"))?;
        Ok(Some(response.into()))
    }
}

fn complete(
    config: &Config,
    cwd: &AbsolutePathBuf,
    params: WorkflowCompleteParams,
) -> anyhow::Result<Option<WorkflowCompleteResponse>> {
    let Some(command) =
        discover_workflow_commands_bounded(config.codex_home.as_path(), cwd.as_path())?
            .into_iter()
            .find(|command| command.id == params.id)
    else {
        return Ok(None);
    };
    let request = CompletionRequest {
        input: params.input,
        active_field: params.active_field,
        prefix: params.prefix,
        mode: match params.mode {
            WorkflowCompletionMode::Field => CompletionMode::Field,
            WorkflowCompletionMode::Value => CompletionMode::Value,
        },
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let receipts = config.codex_home.join(".workflow-management/receipts");
    let managed = if receipts.as_path().is_dir()
        && fs::read_dir(receipts.as_path())?.next().is_some()
    {
        let service =
            ManagedWorkflowService::new(&config.codex_home, &config.codex_home.join("workflows"))?;
        if service.list_installed()?.into_iter().any(|record| {
            record.id == command.id
                && command.workflow_dir
                    == config
                        .codex_home
                        .join("workflows")
                        .join(&record.id)
                        .as_path()
        }) {
            let workspace =
                service.prepare_run_workspace(&command.id, &command.workflow_dir, &cancelled)?;
            workspace.load_executable(&cancelled)?;
            Some(complete_published_workflow_cancellable(
                workspace.root(),
                &request,
                Arc::clone(&cancelled),
            ))
        } else {
            None
        }
    } else {
        None
    };
    let result = managed.unwrap_or_else(|| {
        complete_workflow_cancellable(&command.workflow_dir, &request, Arc::clone(&cancelled))
    });
    Ok(Some(WorkflowCompleteResponse {
        items: result
            .items
            .iter()
            .map(|item| WorkflowCompletionItem {
                value: item.value.clone(),
                description: item.description.clone(),
                insertion: Some(result.insertion_for(&item.value).to_string()),
            })
            .collect(),
        error: result
            .error
            .map(|error| error.chars().take(MAX_ERROR_CHARS).collect()),
    }))
}
