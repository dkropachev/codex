use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowDismissParams;
use codex_app_server_protocol::WorkflowDismissResponse;
use codex_app_server_protocol::WorkflowInstallParams;
use codex_app_server_protocol::WorkflowInstallResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity as ApiReleaseIdentity;
use codex_app_server_protocol::WorkflowResolveSourceParams;
use codex_app_server_protocol::WorkflowResolveSourceResponse;
use codex_app_server_protocol::WorkflowSetPolicyParams;
use codex_app_server_protocol::WorkflowSetPolicyResponse;
use codex_app_server_protocol::WorkflowUninstallParams;
use codex_app_server_protocol::WorkflowUninstallResponse;
use codex_app_server_protocol::WorkflowUpdateParams;
use codex_app_server_protocol::WorkflowUpdateResponse;
use codex_core::config::Config;
use codex_workflows::ManagedWorkflowInstallRequest;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ManagedWorkflowUpdateRequest;
use codex_workflows::WorkflowGitSource;
use codex_workflows::WorkflowReleaseIdentity;
use codex_workflows::WorkflowUpdatePolicy;
use codex_workflows::resolve_workflow_git_release_cancellable;

use super::WorkflowListProcessor;
use super::dependency_runtime::DependencyRuntimePaths;
use super::managed_info;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;

const MAX_MUTATION_ERROR_CHARS: usize = 2_048;

impl WorkflowListProcessor {
    pub(crate) async fn resolve_source(
        &self,
        params: WorkflowResolveSourceParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.ready_for_mutation().await?;
        let source = source_for_host(&params.source, params.cwd.as_deref())?;
        let _slot = self.acquire_source_slot()?;
        let response = mutate(Arc::clone(&self.config), move |_, _| {
            let source = WorkflowGitSource::parse(&source)?;
            let selected =
                resolve_workflow_git_release_cancellable(&source, &AtomicBool::new(false))?;
            Ok(WorkflowResolveSourceResponse {
                release: ApiReleaseIdentity {
                    tag: selected.tag,
                    version: selected.version.map(|version| version.to_string()),
                    commit: selected.advertised_object_id,
                },
            })
        })
        .await?;
        validate_identity(&response.release)?;
        Ok(Some(response.into()))
    }

    pub(crate) async fn install(
        &self,
        params: WorkflowInstallParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let updates = self.ready_for_mutation().await?;
        let source = source_for_host(&params.source, params.cwd.as_deref())?;
        validate_identity(&params.expected_release)?;
        let _slot = self.acquire_source_slot()?;
        let response = mutate(Arc::clone(&self.config), move |service, config| {
            let runtime = DependencyRuntimePaths::from_config(config)?;
            let policy = match params.policy {
                codex_app_server_protocol::WorkflowUpdatePolicy::Prompt => {
                    WorkflowUpdatePolicy::Prompt
                }
                codex_app_server_protocol::WorkflowUpdatePolicy::Automatic => {
                    WorkflowUpdatePolicy::Automatic
                }
                codex_app_server_protocol::WorkflowUpdatePolicy::Manual => {
                    WorkflowUpdatePolicy::Manual
                }
            };
            let installed = service.install_with_policy_expected(
                ManagedWorkflowInstallRequest {
                    source: &source,
                    dependency_runtime: runtime.runtime(config),
                    cancelled: &AtomicBool::new(false),
                },
                policy,
                &release(&params.expected_release),
            )?;
            Ok(WorkflowInstallResponse {
                id: installed.id,
                installed: ApiReleaseIdentity {
                    tag: installed.release.tag,
                    version: installed.release.version.map(|version| version.to_string()),
                    commit: installed.release.advertised_object_id,
                },
                cleanup_pending: installed.cleanup_pending,
            })
        })
        .await?;
        updates.refresh_after_mutation();
        Ok(Some(response.into()))
    }

    pub(crate) async fn update_release(
        &self,
        params: WorkflowUpdateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let updates = self.ready_for_mutation().await?;
        validate_request(
            &params.id,
            &[&params.expected_installed, &params.expected_available],
        )?;
        let _slot = self.acquire_source_slot()?;
        let response = mutate(Arc::clone(&self.config), move |service, config| {
            let runtime = DependencyRuntimePaths::from_config(config)?;
            let installed = service.update(ManagedWorkflowUpdateRequest {
                id: &params.id,
                expected_installed: &release(&params.expected_installed),
                expected_available: &release(&params.expected_available),
                dependency_runtime: runtime.runtime(config),
                cancelled: &AtomicBool::new(false),
            })?;
            Ok(WorkflowUpdateResponse {
                installed: ApiReleaseIdentity {
                    tag: installed.release.tag,
                    version: installed.release.version.map(|version| version.to_string()),
                    commit: installed.release.advertised_object_id,
                },
                cleanup_pending: installed.cleanup_pending,
            })
        })
        .await?;
        updates.refresh_after_mutation();
        Ok(Some(response.into()))
    }

    pub(crate) async fn set_policy(
        &self,
        params: WorkflowSetPolicyParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let updates = self.ready_for_mutation().await?;
        validate_request(&params.id, &[&params.expected_installed])?;
        let response = mutate(Arc::clone(&self.config), move |service, _| {
            let policy = match params.policy {
                codex_app_server_protocol::WorkflowUpdatePolicy::Prompt => {
                    WorkflowUpdatePolicy::Prompt
                }
                codex_app_server_protocol::WorkflowUpdatePolicy::Automatic => {
                    WorkflowUpdatePolicy::Automatic
                }
                codex_app_server_protocol::WorkflowUpdatePolicy::Manual => {
                    WorkflowUpdatePolicy::Manual
                }
            };
            let record = service.set_policy(
                &params.id,
                &release(&params.expected_installed),
                policy,
                &AtomicBool::new(false),
            )?;
            Ok(WorkflowSetPolicyResponse {
                managed: managed_info(&record),
            })
        })
        .await?;
        updates.refresh_after_mutation();
        Ok(Some(response.into()))
    }

    pub(crate) async fn dismiss_release(
        &self,
        params: WorkflowDismissParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let updates = self.ready_for_mutation().await?;
        validate_request(&params.id, &[&params.expected_installed, &params.release])?;
        let response = mutate(Arc::clone(&self.config), move |service, _| {
            let record = service.dismiss_release(
                &params.id,
                &release(&params.expected_installed),
                &release(&params.release),
                &AtomicBool::new(false),
            )?;
            Ok(WorkflowDismissResponse {
                managed: managed_info(&record),
            })
        })
        .await?;
        updates.refresh_after_mutation();
        Ok(Some(response.into()))
    }

    pub(crate) async fn uninstall(
        &self,
        params: WorkflowUninstallParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let updates = self.ready_for_mutation().await?;
        validate_request(&params.id, &[&params.expected_installed])?;
        let response = mutate(Arc::clone(&self.config), move |service, _| {
            let removed = service.uninstall(
                &params.id,
                &release(&params.expected_installed),
                &AtomicBool::new(false),
            )?;
            Ok(WorkflowUninstallResponse {
                cleanup_pending: removed.cleanup_pending,
            })
        })
        .await?;
        updates.refresh_after_mutation();
        Ok(Some(response.into()))
    }
}

async fn mutate<R: Send + 'static>(
    config: Arc<Config>,
    operation: impl FnOnce(&ManagedWorkflowService, &Config) -> anyhow::Result<R> + Send + 'static,
) -> Result<R, JSONRPCErrorError> {
    tokio::task::spawn_blocking(move || {
        let service =
            ManagedWorkflowService::new(&config.codex_home, &config.codex_home.join("workflows"))?;
        operation(&service, config.as_ref())
    })
    .await
    .map_err(|error| internal_error(format!("workflow mutation task failed: {error}")))?
    .map_err(|error| {
        internal_error(
            format!("workflow mutation failed: {error:#}")
                .chars()
                .take(MAX_MUTATION_ERROR_CHARS)
                .collect::<String>(),
        )
    })
}

fn validate_request(id: &str, identities: &[&ApiReleaseIdentity]) -> Result<(), JSONRPCErrorError> {
    if id.is_empty() || id.len() > 240 {
        return Err(invalid_params("workflow ID is empty or too long"));
    }
    for identity in identities {
        validate_identity(identity)?;
    }
    Ok(())
}

fn validate_source(source: &str) -> Result<(), JSONRPCErrorError> {
    if source.is_empty() || source.len() > 4_096 {
        return Err(invalid_params("workflow source is empty or too long"));
    }
    Ok(())
}

fn source_for_host(source: &str, cwd: Option<&str>) -> Result<String, JSONRPCErrorError> {
    validate_source(source)?;
    let Some(cwd) = cwd else {
        return Ok(source.to_string());
    };
    if cwd.len() > 4_096 {
        return Err(invalid_params("workflow cwd is too long"));
    }
    let cwd = PathBuf::from(cwd);
    if !cwd.is_absolute() {
        return Err(invalid_params(
            "workflow cwd must be absolute on the app-server host",
        ));
    }
    if Path::new(source).is_relative() && !source.contains("://") && !source.contains(':') {
        return cwd
            .join(source)
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| invalid_params("workflow source path is not UTF-8"));
    }
    Ok(source.to_string())
}

fn validate_identity(identity: &ApiReleaseIdentity) -> Result<(), JSONRPCErrorError> {
    if identity.commit.len() > 128
        || identity.tag.as_ref().is_some_and(|tag| tag.len() > 512)
        || identity
            .version
            .as_ref()
            .is_some_and(|version| version.len() > 128)
    {
        return Err(invalid_params("workflow release identity is too long"));
    }
    Ok(())
}

fn release(release: &ApiReleaseIdentity) -> WorkflowReleaseIdentity {
    WorkflowReleaseIdentity {
        tag: release.tag.clone(),
        version: release.version.clone(),
        commit: release.commit.clone(),
    }
}
