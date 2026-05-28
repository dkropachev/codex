use std::fs;
use std::sync::Arc;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowAuthoringContextPrepareParams;
use codex_app_server_protocol::WorkflowAuthoringContextPrepareResponse;
use codex_app_server_protocol::WorkflowCommandExecuteParams;
use codex_app_server_protocol::WorkflowCommandExecuteResponse;
use codex_app_server_protocol::WorkflowCommandOptionHint;
use codex_app_server_protocol::WorkflowCommandResponse;
use codex_app_server_protocol::WorkflowConfigReadParams;
use codex_app_server_protocol::WorkflowConfigReadResponse;
use codex_app_server_protocol::WorkflowConfigValues;
use codex_app_server_protocol::WorkflowConfigWriteParams;
use codex_app_server_protocol::WorkflowConfigWriteResponse;
use codex_app_server_protocol::WorkflowDevelopLocation as ApiWorkflowDevelopLocation;
use codex_app_server_protocol::WorkflowDevelopParams;
use codex_app_server_protocol::WorkflowDevelopResponse;
use codex_app_server_protocol::WorkflowDiscardResponse;
use codex_app_server_protocol::WorkflowEditParams;
use codex_app_server_protocol::WorkflowEditResponse;
use codex_app_server_protocol::WorkflowImpactInfo;
use codex_app_server_protocol::WorkflowImpactParams;
use codex_app_server_protocol::WorkflowImpactResponse;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPublishResponse;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRepairParams;
use codex_app_server_protocol::WorkflowRepairResponse;
use codex_app_server_protocol::WorkflowRootInfo;
use codex_app_server_protocol::WorkflowRootKind;
use codex_app_server_protocol::WorkflowRunCancelParams;
use codex_app_server_protocol::WorkflowRunReadParams;
use codex_app_server_protocol::WorkflowRunStartParams;
use codex_app_server_protocol::WorkflowRunWaitParams;
use codex_app_server_protocol::WorkflowStageSessionActionParams;
use codex_app_server_protocol::WorkflowSummary;
use codex_app_server_protocol::WorkflowValidateParams;
use codex_app_server_protocol::WorkflowValidateResponse;
use codex_app_server_protocol::WorkflowValidationCommandResult;
use codex_app_server_protocol::WorkflowValidationFindingInfo;
use codex_app_server_protocol::WorkflowValidationInfo;
use codex_app_server_protocol::WorkflowValidationStatus;
use codex_config::types::WorkflowDefaultLocation;
use codex_config::types::WorkflowsConfigToml;
use codex_core::config::Config;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowCommandProgress;
use codex_workflows::WorkflowCommandProgressHandler;
use codex_workflows::WorkflowConfigCommand;
use codex_workflows::WorkflowDevelopLocation;
use codex_workflows::WorkflowDevelopRequest;
use codex_workflows::discover_workflows_for_context;
use codex_workflows::execute_workflow_command;
use codex_workflows::parse_mention_target;
use codex_workflows::parse_workflow_command_with_workflows;
use codex_workflows::resolve_workflow_for_context;
use codex_workflows::workflow_impact;
use serde::Deserialize;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;
use crate::outgoing_message::OutgoingMessageSender;
use codex_app_server_protocol::ServerNotification;

use super::workflow_run_manager::WorkflowRunManager;
use super::workflow_run_manager::WorkflowRunStartArgs;

#[derive(Clone)]
pub(crate) struct WorkflowRequestProcessor {
    config: Arc<Config>,
    config_manager: ConfigManager,
    outgoing: Arc<OutgoingMessageSender>,
    workflow_runs: WorkflowRunManager,
    workflow_app_server_url: Option<String>,
}

impl WorkflowRequestProcessor {
    pub(crate) fn new(
        config: Arc<Config>,
        config_manager: ConfigManager,
        outgoing: Arc<OutgoingMessageSender>,
        workflow_app_server_url: Option<String>,
    ) -> Self {
        let workflow_runs = WorkflowRunManager::new(Arc::clone(&outgoing));
        Self {
            config,
            config_manager,
            outgoing,
            workflow_runs,
            workflow_app_server_url,
        }
    }

    pub(crate) async fn list(
        &self,
        params: WorkflowListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let roots = codex_workflows::workflow_roots(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
        )
        .into_iter()
        .map(root_to_api)
        .collect();
        let workflows = self.discover_api_workflows(params.stage_session_id.as_deref())?;
        Ok(Some(WorkflowListResponse { roots, workflows }.into()))
    }

    pub(crate) async fn read(
        &self,
        params: WorkflowReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let workflow =
            self.resolve_workflow(params.id, params.target, params.stage_session_id.as_deref())?;
        let workflow_yaml = fs::read_to_string(&workflow.workflow_yaml_path).map_err(|err| {
            internal_error(format!(
                "failed to read workflow metadata {}: {err}",
                workflow.workflow_yaml_path.display()
            ))
        })?;
        let readme = fs::read_to_string(workflow.path.join("README.md")).ok();
        Ok(Some(
            WorkflowReadResponse {
                workflow,
                workflow_yaml,
                readme,
            }
            .into(),
        ))
    }

    pub(crate) async fn impact(
        &self,
        params: WorkflowImpactParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let workflow = self.resolve_workflow(
            params.id,
            /*target*/ None,
            params.stage_session_id.as_deref(),
        )?;
        let impact = workflow_impact(&workflow_to_core(&workflow))
            .map_err(|err| internal_error(format!("failed to inspect workflow impact: {err}")))?;
        Ok(Some(
            WorkflowImpactResponse {
                impact: impact_to_api(impact),
            }
            .into(),
        ))
    }

    pub(crate) async fn develop(
        &self,
        params: WorkflowDevelopParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(
            WorkflowCommand::Develop {
                request: WorkflowDevelopRequest {
                    description: params.description,
                    id: params.id,
                    command: params.command,
                    title: params.title,
                    location: params.location.map(location_from_api),
                },
            },
            params.stage_session_id,
        )
        .map(|response: WorkflowDevelopResponse| Some(response.into()))
    }

    pub(crate) async fn edit(
        &self,
        params: WorkflowEditParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(
            WorkflowCommand::Edit {
                id: params.id,
                instruction: params.instruction,
            },
            params.stage_session_id,
        )
        .map(|response: WorkflowEditResponse| Some(response.into()))
    }

    pub(crate) async fn start_run(
        &self,
        params: WorkflowRunStartParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let workflow = self.resolve_workflow(
            params.id,
            /*target*/ None,
            params.stage_session_id.as_deref(),
        )?;
        self.workflow_runs
            .start(WorkflowRunStartArgs {
                config: Arc::clone(&self.config),
                workflow_id: workflow.id,
                input: params.input,
                thread_id: params.thread_id,
                stage_session_id: params.stage_session_id,
                approval_handling: params.approval_handling,
                app_server_url: self.workflow_app_server_url.clone(),
            })
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn read_run(
        &self,
        params: WorkflowRunReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.workflow_runs
            .read(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn wait_run(
        &self,
        params: WorkflowRunWaitParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.workflow_runs
            .wait(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn cancel_run(
        &self,
        params: WorkflowRunCancelParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.workflow_runs
            .cancel(params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(crate) async fn validate(
        &self,
        params: WorkflowValidateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(
            WorkflowCommand::Validate { id: params.id },
            params.stage_session_id,
        )
        .map(|response: WorkflowValidateResponse| Some(response.into()))
    }

    pub(crate) async fn repair(
        &self,
        params: WorkflowRepairParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let run_id = Uuid::new_v4().to_string();
        let outgoing = Arc::clone(&self.outgoing);
        let progress = |event: WorkflowCommandProgress| {
            outgoing.try_send_server_notification(ServerNotification::WorkflowRunProgress(
                codex_app_server_protocol::WorkflowProgressNotification {
                    run_id: run_id.clone(),
                    thread_id: None,
                    message: event.message,
                    data: event.data,
                    status: None,
                },
            ));
        };
        let response: WorkflowCommandResponse = self.execute_with_progress(
            WorkflowCommand::Fix { id: params.id },
            params.stage_session_id,
            Some(&progress),
        )?;
        let payload: WorkflowRepairPayload =
            serde_json::from_value(response.data).map_err(|err| {
                internal_error(format!("failed to decode workflow repair payload: {err}"))
            })?;
        Ok(Some(
            WorkflowRepairResponse {
                message: response.message,
                exit_code: response.exit_code,
                workflow: payload.workflow,
                validation: payload.validation,
                validation_command_results: payload.validation_command_results,
                repair: payload.repair,
            }
            .into(),
        ))
    }

    pub(crate) async fn publish(
        &self,
        params: WorkflowStageSessionActionParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(WorkflowCommand::Publish, Some(params.stage_session_id))
            .map(|response: WorkflowPublishResponse| Some(response.into()))
    }

    pub(crate) async fn discard(
        &self,
        params: WorkflowStageSessionActionParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(WorkflowCommand::Discard, Some(params.stage_session_id))
            .map(|response: WorkflowDiscardResponse| Some(response.into()))
    }

    pub(crate) async fn config_read(
        &self,
        _params: WorkflowConfigReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(
            WorkflowConfigReadResponse {
                config: config_values(&self.config.workflows),
            }
            .into(),
        ))
    }

    pub(crate) async fn config_write(
        &self,
        params: WorkflowConfigWriteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let command = match params.value {
            Some(value) => WorkflowCommand::Config(WorkflowConfigCommand::Set {
                key: params.key,
                value: config_value_to_command_string(value),
            }),
            None => WorkflowCommand::Config(WorkflowConfigCommand::Clear { key: params.key }),
        };
        let _response =
            self.execute::<WorkflowCommandResponse>(command, /*stage_session_id*/ None)?;
        let config = self.load_latest_config().await?;
        Ok(Some(
            WorkflowConfigWriteResponse {
                config: config_values(&config.workflows),
            }
            .into(),
        ))
    }

    pub(crate) async fn command_execute(
        &self,
        params: WorkflowCommandExecuteParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let workflows = discover_workflows_for_context(
            &self.workflow_command_context(params.stage_session_id.clone(), /*progress*/ None),
        )
        .map_err(|err| internal_error(format!("failed to discover workflows: {err}")))?;
        let command = parse_workflow_command_with_workflows(&params.args, &workflows)
            .map_err(|err| invalid_params(format!("invalid workflow command: {err}")))?;
        self.execute(command, params.stage_session_id)
            .map(|response: WorkflowCommandExecuteResponse| Some(response.into()))
    }

    pub(crate) async fn authoring_context_prepare(
        &self,
        params: WorkflowAuthoringContextPrepareParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let roots = codex_workflows::workflow_roots(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
        )
        .into_iter()
        .map(root_to_api)
        .collect();
        Ok(Some(
            WorkflowAuthoringContextPrepareResponse {
                roots,
                workflows: self.discover_api_workflows(params.stage_session_id.as_deref())?,
                config: config_values(&self.config.workflows),
            }
            .into(),
        ))
    }

    fn discover_api_workflows(
        &self,
        stage_session_id: Option<&str>,
    ) -> Result<Vec<WorkflowSummary>, JSONRPCErrorError> {
        discover_workflows_for_context(&self.workflow_command_context(
            stage_session_id.map(ToString::to_string),
            /*progress*/ None,
        ))
        .map(|workflows| workflows.into_iter().map(summary_to_api).collect())
        .map_err(|err| internal_error(format!("failed to discover workflows: {err}")))
    }

    fn resolve_workflow(
        &self,
        id: String,
        target: Option<String>,
        stage_session_id: Option<&str>,
    ) -> Result<WorkflowSummary, JSONRPCErrorError> {
        if let Some(target) = target {
            let parsed = parse_mention_target(&target)
                .map_err(|err| invalid_params(format!("invalid workflow target: {err}")))?;
            return self
                .discover_api_workflows(stage_session_id)?
                .into_iter()
                .find(|workflow| workflow.id == parsed.id && workflow.root_path == parsed.root_path)
                .ok_or_else(|| invalid_params("workflow target was not found"));
        }

        resolve_workflow_for_context(
            &self.workflow_command_context(
                stage_session_id.map(ToString::to_string),
                /*progress*/ None,
            ),
            &id,
        )
        .map(summary_to_api)
        .map_err(|err| invalid_params(format!("failed to resolve workflow: {err}")))
    }

    fn execute<T>(
        &self,
        command: WorkflowCommand,
        stage_session_id: Option<String>,
    ) -> Result<T, JSONRPCErrorError>
    where
        T: From<WorkflowCommandResponse>,
    {
        self.execute_with_progress(command, stage_session_id, /*progress*/ None)
    }

    fn execute_with_progress<'a, T>(
        &'a self,
        command: WorkflowCommand,
        stage_session_id: Option<String>,
        progress: Option<&'a WorkflowCommandProgressHandler<'a>>,
    ) -> Result<T, JSONRPCErrorError>
    where
        T: From<WorkflowCommandResponse>,
    {
        execute_workflow_command(
            self.workflow_command_context(stage_session_id, progress),
            command,
        )
        .map(|output| WorkflowCommandResponse {
            message: output.message,
            data: output.data,
            exit_code: output.exit_code,
        })
        .map(T::from)
        .map_err(|err| internal_error(format!("workflow command failed: {err}")))
    }

    fn workflow_command_context<'a>(
        &'a self,
        stage_session_id: Option<String>,
        progress: Option<&'a WorkflowCommandProgressHandler<'a>>,
    ) -> WorkflowCommandContext<'a> {
        WorkflowCommandContext {
            codex_home: self.config.codex_home.as_path(),
            cwd: self.config.cwd.as_path(),
            config: &self.config.workflows,
            codex_self_exe: self.config.codex_self_exe.clone(),
            stage_session_id,
            progress,
            runtime_event_handler: None,
            runtime: Default::default(),
        }
    }

    async fn load_latest_config(&self) -> Result<Config, JSONRPCErrorError> {
        self.config_manager
            .load_latest_config(Some(self.config.cwd.to_path_buf()))
            .await
            .map_err(|err| internal_error(format!("failed to reload config: {err}")))
    }
}

fn summary_to_api(summary: codex_workflows::WorkflowSummary) -> WorkflowSummary {
    WorkflowSummary {
        id: summary.id,
        command: summary.command,
        title: summary.title,
        user_description: summary.user_description,
        search_terms: summary.search_terms,
        command_option_hints: summary
            .command_option_hints
            .into_iter()
            .map(|hint| WorkflowCommandOptionHint {
                display: hint.display,
                description: hint.description,
            })
            .collect(),
        root_label: summary.root_label,
        root_kind: root_kind_to_api(summary.root_kind),
        root_path: summary.root_path,
        path: summary.path,
        workflow_yaml_path: summary.workflow_yaml_path,
        mention_target: summary.mention_target,
        validation: validation_to_api(summary.validation),
        repair_mode: summary.repair_mode,
    }
}

fn workflow_to_core(summary: &WorkflowSummary) -> codex_workflows::WorkflowSummary {
    codex_workflows::WorkflowSummary {
        id: summary.id.clone(),
        command: summary.command.clone(),
        title: summary.title.clone(),
        user_description: summary.user_description.clone(),
        search_terms: summary.search_terms.clone(),
        command_option_hints: summary
            .command_option_hints
            .iter()
            .map(|hint| codex_workflows::WorkflowCommandOptionHint {
                display: hint.display.clone(),
                description: hint.description.clone(),
            })
            .collect(),
        root_label: summary.root_label.clone(),
        root_kind: root_kind_from_api(summary.root_kind),
        root_path: summary.root_path.clone(),
        path: summary.path.clone(),
        workflow_yaml_path: summary.workflow_yaml_path.clone(),
        mention_target: summary.mention_target.clone(),
        validation: codex_workflows::WorkflowValidation::from_findings(
            summary
                .validation
                .findings
                .iter()
                .cloned()
                .map(validation_finding_from_api)
                .collect(),
        ),
        repair_mode: summary.repair_mode.clone(),
    }
}

fn root_to_api(root: codex_workflows::WorkflowRoot) -> WorkflowRootInfo {
    WorkflowRootInfo {
        kind: root_kind_to_api(root.kind),
        label: root.label,
        path: root.path,
    }
}

fn location_from_api(location: ApiWorkflowDevelopLocation) -> WorkflowDevelopLocation {
    match location {
        ApiWorkflowDevelopLocation::Project => WorkflowDevelopLocation::Project,
        ApiWorkflowDevelopLocation::Global => WorkflowDevelopLocation::Global,
    }
}

pub(crate) fn validation_to_api(
    validation: codex_workflows::WorkflowValidation,
) -> WorkflowValidationInfo {
    WorkflowValidationInfo {
        status: match validation.status {
            codex_workflows::WorkflowValidationStatus::Valid => WorkflowValidationStatus::Valid,
            codex_workflows::WorkflowValidationStatus::Invalid => WorkflowValidationStatus::Invalid,
        },
        findings: validation
            .findings
            .into_iter()
            .map(validation_finding_to_api)
            .collect(),
    }
}

pub(crate) fn validation_finding_to_api(
    finding: codex_workflows::WorkflowValidationFinding,
) -> WorkflowValidationFindingInfo {
    match finding {
        codex_workflows::WorkflowValidationFinding::WorkflowSpecReadFailed { path, error } => {
            WorkflowValidationFindingInfo::WorkflowSpecReadFailed { path, error }
        }
        codex_workflows::WorkflowValidationFinding::WorkflowIdMismatch {
            path,
            expected_id,
            actual_id,
        } => WorkflowValidationFindingInfo::WorkflowIdMismatch {
            path,
            expected_id,
            actual_id,
        },
        codex_workflows::WorkflowValidationFinding::MissingFile { path } => {
            WorkflowValidationFindingInfo::MissingFile { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingDirectory { path } => {
            WorkflowValidationFindingInfo::MissingDirectory { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingGitRepository { path } => {
            WorkflowValidationFindingInfo::MissingGitRepository { path }
        }
        codex_workflows::WorkflowValidationFinding::WorkflowPathEscapesRoot {
            workflow_path,
            root_path,
        } => WorkflowValidationFindingInfo::WorkflowPathEscapesRoot {
            workflow_path,
            root_path,
        },
        codex_workflows::WorkflowValidationFinding::MissingDocumentHeading { path, heading } => {
            WorkflowValidationFindingInfo::MissingDocumentHeading { path, heading }
        }
        codex_workflows::WorkflowValidationFinding::EmptyDocumentSection { path, heading } => {
            WorkflowValidationFindingInfo::EmptyDocumentSection { path, heading }
        }
        codex_workflows::WorkflowValidationFinding::PackageManifestParseFailed { path, error } => {
            WorkflowValidationFindingInfo::PackageManifestParseFailed { path, error }
        }
        codex_workflows::WorkflowValidationFinding::InvalidPackageManifestField {
            path,
            field,
            expected,
        } => WorkflowValidationFindingInfo::InvalidPackageManifestField {
            path,
            field,
            expected,
        },
        codex_workflows::WorkflowValidationFinding::MissingPackageScript { path, script } => {
            WorkflowValidationFindingInfo::MissingPackageScript { path, script }
        }
        codex_workflows::WorkflowValidationFinding::NonBunPackageScript {
            path,
            script,
            command,
        } => WorkflowValidationFindingInfo::NonBunPackageScript {
            path,
            script,
            command,
        },
        codex_workflows::WorkflowValidationFinding::DisallowedPackageDependency {
            path,
            package_name,
        } => WorkflowValidationFindingInfo::DisallowedPackageDependency { path, package_name },
        codex_workflows::WorkflowValidationFinding::LatestPackageDependency {
            path,
            package_name,
            field,
        } => WorkflowValidationFindingInfo::LatestPackageDependency {
            path,
            package_name,
            field,
        },
        codex_workflows::WorkflowValidationFinding::UndeclaredPackageImport {
            path,
            specifier,
            package_name,
        } => WorkflowValidationFindingInfo::UndeclaredPackageImport {
            path,
            specifier,
            package_name,
        },
        codex_workflows::WorkflowValidationFinding::DisallowedNodeRuntimeImport {
            path,
            specifier,
        } => WorkflowValidationFindingInfo::DisallowedNodeRuntimeImport { path, specifier },
        codex_workflows::WorkflowValidationFinding::DisallowedWorkflowRuntimeFile { path } => {
            WorkflowValidationFindingInfo::DisallowedWorkflowRuntimeFile { path }
        }
        codex_workflows::WorkflowValidationFinding::UnusedPackageDependency {
            path,
            package_name,
        } => WorkflowValidationFindingInfo::UnusedPackageDependency { path, package_name },
        codex_workflows::WorkflowValidationFinding::InvalidWorkflowDependencyMetadata {
            path,
            field,
        } => WorkflowValidationFindingInfo::InvalidWorkflowDependencyMetadata { path, field },
        codex_workflows::WorkflowValidationFinding::WorkflowDependencyMetadataMismatch {
            path,
            package_name,
            source,
            target,
        } => WorkflowValidationFindingInfo::WorkflowDependencyMetadataMismatch {
            path,
            package_name,
            source,
            target,
        },
        codex_workflows::WorkflowValidationFinding::MissingValidationCommands { path } => {
            WorkflowValidationFindingInfo::MissingValidationCommands { path }
        }
        codex_workflows::WorkflowValidationFinding::EmptyValidationCommands { path } => {
            WorkflowValidationFindingInfo::EmptyValidationCommands { path }
        }
        codex_workflows::WorkflowValidationFinding::InvalidValidationCommands { path } => {
            WorkflowValidationFindingInfo::InvalidValidationCommands { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingBuildValidationCommand { path } => {
            WorkflowValidationFindingInfo::MissingBuildValidationCommand { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingTestValidationCommand { path } => {
            WorkflowValidationFindingInfo::MissingTestValidationCommand { path }
        }
        codex_workflows::WorkflowValidationFinding::NonBunValidationCommand { path, command } => {
            WorkflowValidationFindingInfo::NonBunValidationCommand { path, command }
        }
        codex_workflows::WorkflowValidationFinding::MissingContractSmoke { path } => {
            WorkflowValidationFindingInfo::MissingContractSmoke { path }
        }
        codex_workflows::WorkflowValidationFinding::InvalidContractSmoke { path } => {
            WorkflowValidationFindingInfo::InvalidContractSmoke { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingCoverageMetadata { path } => {
            WorkflowValidationFindingInfo::MissingCoverageMetadata { path }
        }
        codex_workflows::WorkflowValidationFinding::MissingCoverageKey { path, key } => {
            WorkflowValidationFindingInfo::MissingCoverageKey { path, key }
        }
        codex_workflows::WorkflowValidationFinding::InvalidCoverageKeyType { path, key } => {
            WorkflowValidationFindingInfo::InvalidCoverageKeyType { path, key }
        }
        codex_workflows::WorkflowValidationFinding::CoverageKeyMustBeTrue { path, key } => {
            WorkflowValidationFindingInfo::CoverageKeyMustBeTrue { path, key }
        }
        codex_workflows::WorkflowValidationFinding::MissingCoverageMarker { path, key } => {
            WorkflowValidationFindingInfo::MissingCoverageMarker { path, key }
        }
        codex_workflows::WorkflowValidationFinding::ScaffoldEchoSource { path } => {
            WorkflowValidationFindingInfo::ScaffoldEchoSource { path }
        }
        codex_workflows::WorkflowValidationFinding::PlaceholderWorkflowTest { path, reason } => {
            WorkflowValidationFindingInfo::PlaceholderWorkflowTest { path, reason }
        }
        codex_workflows::WorkflowValidationFinding::RawDevelopFlagsInMetadata { path, field } => {
            WorkflowValidationFindingInfo::RawDevelopFlagsInMetadata { path, field }
        }
        codex_workflows::WorkflowValidationFinding::CodeOutsideSrc { paths } => {
            WorkflowValidationFindingInfo::CodeOutsideSrc { paths }
        }
        codex_workflows::WorkflowValidationFinding::TestsOutsideSrcTests { paths } => {
            WorkflowValidationFindingInfo::TestsOutsideSrcTests { paths }
        }
        codex_workflows::WorkflowValidationFinding::DatabasesOutsideState { paths } => {
            WorkflowValidationFindingInfo::DatabasesOutsideState { paths }
        }
        codex_workflows::WorkflowValidationFinding::RuntimeStateGitignoreMissing {
            path,
            patterns,
        } => WorkflowValidationFindingInfo::RuntimeStateGitignoreMissing { path, patterns },
        codex_workflows::WorkflowValidationFinding::TrackedRuntimeStateFiles { paths } => {
            WorkflowValidationFindingInfo::TrackedRuntimeStateFiles { paths }
        }
        codex_workflows::WorkflowValidationFinding::AmbiguousWorkflowOutputSchema {
            path,
            schema_path,
        } => WorkflowValidationFindingInfo::AmbiguousWorkflowOutputSchema { path, schema_path },
        codex_workflows::WorkflowValidationFinding::ValidationCommandFailed {
            command,
            exit_code,
            stdout,
            stderr,
        } => WorkflowValidationFindingInfo::ValidationCommandFailed {
            command,
            exit_code,
            stdout,
            stderr,
        },
        codex_workflows::WorkflowValidationFinding::WorkflowApiContractExtractionFailed {
            path,
            error,
        } => WorkflowValidationFindingInfo::WorkflowApiContractExtractionFailed { path, error },
        codex_workflows::WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command,
            error,
            stdout,
            stderr,
        } => WorkflowValidationFindingInfo::WorkflowApiContractSmokeFailed {
            command,
            error,
            stdout,
            stderr,
        },
    }
}

fn validation_finding_from_api(
    finding: WorkflowValidationFindingInfo,
) -> codex_workflows::WorkflowValidationFinding {
    match finding {
        WorkflowValidationFindingInfo::WorkflowSpecReadFailed { path, error } => {
            codex_workflows::WorkflowValidationFinding::WorkflowSpecReadFailed { path, error }
        }
        WorkflowValidationFindingInfo::WorkflowIdMismatch {
            path,
            expected_id,
            actual_id,
        } => codex_workflows::WorkflowValidationFinding::WorkflowIdMismatch {
            path,
            expected_id,
            actual_id,
        },
        WorkflowValidationFindingInfo::MissingFile { path } => {
            codex_workflows::WorkflowValidationFinding::MissingFile { path }
        }
        WorkflowValidationFindingInfo::MissingDirectory { path } => {
            codex_workflows::WorkflowValidationFinding::MissingDirectory { path }
        }
        WorkflowValidationFindingInfo::MissingGitRepository { path } => {
            codex_workflows::WorkflowValidationFinding::MissingGitRepository { path }
        }
        WorkflowValidationFindingInfo::WorkflowPathEscapesRoot {
            workflow_path,
            root_path,
        } => codex_workflows::WorkflowValidationFinding::WorkflowPathEscapesRoot {
            workflow_path,
            root_path,
        },
        WorkflowValidationFindingInfo::MissingDocumentHeading { path, heading } => {
            codex_workflows::WorkflowValidationFinding::MissingDocumentHeading { path, heading }
        }
        WorkflowValidationFindingInfo::EmptyDocumentSection { path, heading } => {
            codex_workflows::WorkflowValidationFinding::EmptyDocumentSection { path, heading }
        }
        WorkflowValidationFindingInfo::PackageManifestParseFailed { path, error } => {
            codex_workflows::WorkflowValidationFinding::PackageManifestParseFailed { path, error }
        }
        WorkflowValidationFindingInfo::InvalidPackageManifestField {
            path,
            field,
            expected,
        } => codex_workflows::WorkflowValidationFinding::InvalidPackageManifestField {
            path,
            field,
            expected,
        },
        WorkflowValidationFindingInfo::MissingPackageScript { path, script } => {
            codex_workflows::WorkflowValidationFinding::MissingPackageScript { path, script }
        }
        WorkflowValidationFindingInfo::NonBunPackageScript {
            path,
            script,
            command,
        } => codex_workflows::WorkflowValidationFinding::NonBunPackageScript {
            path,
            script,
            command,
        },
        WorkflowValidationFindingInfo::DisallowedPackageDependency { path, package_name } => {
            codex_workflows::WorkflowValidationFinding::DisallowedPackageDependency {
                path,
                package_name,
            }
        }
        WorkflowValidationFindingInfo::LatestPackageDependency {
            path,
            package_name,
            field,
        } => codex_workflows::WorkflowValidationFinding::LatestPackageDependency {
            path,
            package_name,
            field,
        },
        WorkflowValidationFindingInfo::UndeclaredPackageImport {
            path,
            specifier,
            package_name,
        } => codex_workflows::WorkflowValidationFinding::UndeclaredPackageImport {
            path,
            specifier,
            package_name,
        },
        WorkflowValidationFindingInfo::DisallowedNodeRuntimeImport { path, specifier } => {
            codex_workflows::WorkflowValidationFinding::DisallowedNodeRuntimeImport {
                path,
                specifier,
            }
        }
        WorkflowValidationFindingInfo::DisallowedWorkflowRuntimeFile { path } => {
            codex_workflows::WorkflowValidationFinding::DisallowedWorkflowRuntimeFile { path }
        }
        WorkflowValidationFindingInfo::UnusedPackageDependency { path, package_name } => {
            codex_workflows::WorkflowValidationFinding::UnusedPackageDependency {
                path,
                package_name,
            }
        }
        WorkflowValidationFindingInfo::InvalidWorkflowDependencyMetadata { path, field } => {
            codex_workflows::WorkflowValidationFinding::InvalidWorkflowDependencyMetadata {
                path,
                field,
            }
        }
        WorkflowValidationFindingInfo::WorkflowDependencyMetadataMismatch {
            path,
            package_name,
            source,
            target,
        } => codex_workflows::WorkflowValidationFinding::WorkflowDependencyMetadataMismatch {
            path,
            package_name,
            source,
            target,
        },
        WorkflowValidationFindingInfo::MissingValidationCommands { path } => {
            codex_workflows::WorkflowValidationFinding::MissingValidationCommands { path }
        }
        WorkflowValidationFindingInfo::EmptyValidationCommands { path } => {
            codex_workflows::WorkflowValidationFinding::EmptyValidationCommands { path }
        }
        WorkflowValidationFindingInfo::InvalidValidationCommands { path } => {
            codex_workflows::WorkflowValidationFinding::InvalidValidationCommands { path }
        }
        WorkflowValidationFindingInfo::MissingBuildValidationCommand { path } => {
            codex_workflows::WorkflowValidationFinding::MissingBuildValidationCommand { path }
        }
        WorkflowValidationFindingInfo::MissingTestValidationCommand { path } => {
            codex_workflows::WorkflowValidationFinding::MissingTestValidationCommand { path }
        }
        WorkflowValidationFindingInfo::NonBunValidationCommand { path, command } => {
            codex_workflows::WorkflowValidationFinding::NonBunValidationCommand { path, command }
        }
        WorkflowValidationFindingInfo::MissingContractSmoke { path } => {
            codex_workflows::WorkflowValidationFinding::MissingContractSmoke { path }
        }
        WorkflowValidationFindingInfo::InvalidContractSmoke { path } => {
            codex_workflows::WorkflowValidationFinding::InvalidContractSmoke { path }
        }
        WorkflowValidationFindingInfo::MissingCoverageMetadata { path } => {
            codex_workflows::WorkflowValidationFinding::MissingCoverageMetadata { path }
        }
        WorkflowValidationFindingInfo::MissingCoverageKey { path, key } => {
            codex_workflows::WorkflowValidationFinding::MissingCoverageKey { path, key }
        }
        WorkflowValidationFindingInfo::InvalidCoverageKeyType { path, key } => {
            codex_workflows::WorkflowValidationFinding::InvalidCoverageKeyType { path, key }
        }
        WorkflowValidationFindingInfo::CoverageKeyMustBeTrue { path, key } => {
            codex_workflows::WorkflowValidationFinding::CoverageKeyMustBeTrue { path, key }
        }
        WorkflowValidationFindingInfo::MissingCoverageMarker { path, key } => {
            codex_workflows::WorkflowValidationFinding::MissingCoverageMarker { path, key }
        }
        WorkflowValidationFindingInfo::ScaffoldEchoSource { path } => {
            codex_workflows::WorkflowValidationFinding::ScaffoldEchoSource { path }
        }
        WorkflowValidationFindingInfo::PlaceholderWorkflowTest { path, reason } => {
            codex_workflows::WorkflowValidationFinding::PlaceholderWorkflowTest { path, reason }
        }
        WorkflowValidationFindingInfo::RawDevelopFlagsInMetadata { path, field } => {
            codex_workflows::WorkflowValidationFinding::RawDevelopFlagsInMetadata { path, field }
        }
        WorkflowValidationFindingInfo::CodeOutsideSrc { paths } => {
            codex_workflows::WorkflowValidationFinding::CodeOutsideSrc { paths }
        }
        WorkflowValidationFindingInfo::TestsOutsideSrcTests { paths } => {
            codex_workflows::WorkflowValidationFinding::TestsOutsideSrcTests { paths }
        }
        WorkflowValidationFindingInfo::DatabasesOutsideState { paths } => {
            codex_workflows::WorkflowValidationFinding::DatabasesOutsideState { paths }
        }
        WorkflowValidationFindingInfo::RuntimeStateGitignoreMissing { path, patterns } => {
            codex_workflows::WorkflowValidationFinding::RuntimeStateGitignoreMissing {
                path,
                patterns,
            }
        }
        WorkflowValidationFindingInfo::TrackedRuntimeStateFiles { paths } => {
            codex_workflows::WorkflowValidationFinding::TrackedRuntimeStateFiles { paths }
        }
        WorkflowValidationFindingInfo::AmbiguousWorkflowOutputSchema { path, schema_path } => {
            codex_workflows::WorkflowValidationFinding::AmbiguousWorkflowOutputSchema {
                path,
                schema_path,
            }
        }
        WorkflowValidationFindingInfo::ValidationCommandFailed {
            command,
            exit_code,
            stdout,
            stderr,
        } => codex_workflows::WorkflowValidationFinding::ValidationCommandFailed {
            command,
            exit_code,
            stdout,
            stderr,
        },
        WorkflowValidationFindingInfo::WorkflowApiContractExtractionFailed { path, error } => {
            codex_workflows::WorkflowValidationFinding::WorkflowApiContractExtractionFailed {
                path,
                error,
            }
        }
        WorkflowValidationFindingInfo::WorkflowApiContractSmokeFailed {
            command,
            error,
            stdout,
            stderr,
        } => codex_workflows::WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command,
            error,
            stdout,
            stderr,
        },
    }
}

fn impact_to_api(impact: codex_workflows::WorkflowImpact) -> WorkflowImpactInfo {
    WorkflowImpactInfo {
        id: impact.id,
        path: impact.path,
        dependencies: impact.dependencies,
        dev_dependencies: impact.dev_dependencies,
        git_status: impact.git_status,
    }
}

fn root_kind_to_api(kind: codex_workflows::WorkflowRootKind) -> WorkflowRootKind {
    match kind {
        codex_workflows::WorkflowRootKind::Global => WorkflowRootKind::Global,
        codex_workflows::WorkflowRootKind::Project => WorkflowRootKind::Project,
        codex_workflows::WorkflowRootKind::SearchPath => WorkflowRootKind::SearchPath,
    }
}

fn root_kind_from_api(kind: WorkflowRootKind) -> codex_workflows::WorkflowRootKind {
    match kind {
        WorkflowRootKind::Global => codex_workflows::WorkflowRootKind::Global,
        WorkflowRootKind::Project => codex_workflows::WorkflowRootKind::Project,
        WorkflowRootKind::SearchPath => codex_workflows::WorkflowRootKind::SearchPath,
    }
}

fn config_values(config: &WorkflowsConfigToml) -> WorkflowConfigValues {
    WorkflowConfigValues {
        search_paths: config.search_paths.clone().unwrap_or_default(),
        default_location: match config.default_location.unwrap_or_default() {
            WorkflowDefaultLocation::Global => "global".to_string(),
            WorkflowDefaultLocation::Project => "project".to_string(),
        },
        repair_mode: config
            .repair_mode
            .clone()
            .unwrap_or_else(|| codex_workflows::DEFAULT_REPAIR_MODE.to_string()),
        max_repair_cycles: config
            .max_repair_cycles
            .unwrap_or(codex_workflows::DEFAULT_MAX_REPAIR_CYCLES),
        dependency_update_policy: config
            .dependency_update_policy
            .clone()
            .unwrap_or_else(|| "locked".to_string()),
        commit_policy: config
            .commit_policy
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
        validation_profile: config
            .validation_profile
            .clone()
            .unwrap_or_else(|| "default".to_string()),
    }
}

fn config_value_to_command_string(value: JsonValue) -> String {
    match value {
        JsonValue::String(value) => value,
        JsonValue::Array(values) => values
            .into_iter()
            .map(|value| match value {
                JsonValue::String(value) => value,
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowRepairPayload {
    workflow: WorkflowSummary,
    validation: WorkflowValidationInfo,
    validation_command_results: Vec<WorkflowValidationCommandResult>,
    repair: codex_app_server_protocol::WorkflowRepairResult,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_manager::ConfigManager;
    use codex_app_server_protocol::ClientResponsePayload;
    use codex_arg0::Arg0DispatchPaths;
    use codex_config::CloudRequirementsLoader;
    use codex_config::LoaderOverrides;
    use codex_config::StaticThreadConfigLoader;
    use codex_core::config::ConfigBuilder;
    use codex_core::config::ConfigOverrides;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn write_test_bun_stub(workflow_dir: &std::path::Path) {
        let bin_dir = workflow_dir.join("node_modules/.bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let bun_path = if cfg!(windows) {
            bin_dir.join("bun.cmd")
        } else {
            bin_dir.join("bun")
        };
        let contents = if cfg!(windows) {
            "@echo off\r\necho %* | findstr /C:\"process.exit(1)\" >nul && (\r\n  echo out\r\n  echo err 1>&2\r\n  exit /b 1\r\n)\r\necho {\"ok\":true}\r\nexit /b 0\r\n"
        } else {
            "#!/bin/sh\ncase \"$*\" in *\"process.exit(1)\"*) printf 'out\\n'; printf 'err\\n' >&2; exit 1;; *) printf '{\"ok\":true}\\n'; exit 0;; esac\n"
        };
        fs::write(&bun_path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bun_path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn write_broken_workflow_fixture(workflow_dir: &std::path::Path) {
        write_test_bun_stub(workflow_dir);
        fs::write(
            workflow_dir.join("README.md"),
            "# Broken\n\n## Usage\n\n## Workflow Runtime\n",
        )
        .unwrap();
        fs::write(workflow_dir.join("DESIGN.md"), "# Broken Design\n").unwrap();
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "broken",
  "private": true,
  "type": "module"
}
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.ts"),
            r#"import type { WorkflowContext } from "@openai/codex-sdk/workflow";

export interface WorkflowInput { input?: string; }
export interface WorkflowOutput { ok: boolean; input: WorkflowInput; }
export const WorkflowOutput = { toTuiMarkdown() { return { markdown: "done" }; } };
export default async function run(_ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true, input }; }
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.load.test.ts"),
            "// workflow-covers: load\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.autocomplete.test.ts"),
            "// workflow-covers: autocomplete\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {};\n",
        )
        .unwrap();
        codex_workflows::write_workflow_spec(
            &workflow_dir.join("workflow.yaml"),
            &codex_workflows::WorkflowSpec {
                id: "broken/other".to_string(),
                validation: serde_json::json!({
                    "commands": ["exit 0"],
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn workflow_validation_finding_new_app_server_variants_round_trip() {
        let findings = vec![
            codex_workflows::WorkflowValidationFinding::RuntimeStateGitignoreMissing {
                path: "state/.gitignore".into(),
                patterns: vec!["*".to_string(), "!.gitignore".to_string()],
            },
            codex_workflows::WorkflowValidationFinding::TrackedRuntimeStateFiles {
                paths: vec!["state/cache.db".into(), "state/logs/session.jsonl".into()],
            },
            codex_workflows::WorkflowValidationFinding::AmbiguousWorkflowOutputSchema {
                path: "src/workflow.ts".into(),
                schema_path: "WorkflowOutput.schema".to_string(),
            },
            codex_workflows::WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: "npm run contract-smoke".to_string(),
                error: "contract smoke failed".to_string(),
                stdout: "stdout".to_string(),
                stderr: "stderr".to_string(),
            },
        ];

        let round_tripped = findings
            .iter()
            .cloned()
            .map(validation_finding_to_api)
            .map(validation_finding_from_api)
            .collect::<Vec<_>>();

        assert_eq!(round_tripped, findings);
    }

    #[tokio::test]
    async fn workflow_repair_rpc_returns_structured_repair_results() {
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let workflow_dir = home.path().join("workflows/broken/fix");
        fs::create_dir_all(&workflow_dir).unwrap();
        write_broken_workflow_fixture(&workflow_dir);

        let mut config = ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                cwd: Some(cwd.path().to_path_buf()),
                ..Default::default()
            })
            .build()
            .await
            .unwrap();
        config.workflows.commit_policy = Some("manual".to_string());

        let config_manager = ConfigManager::new(
            home.path().to_path_buf(),
            Vec::new(),
            LoaderOverrides::default(),
            CloudRequirementsLoader::default(),
            Arg0DispatchPaths::default(),
            Arc::new(StaticThreadConfigLoader::new(Vec::new())),
        );
        let (outgoing_tx, mut outgoing_rx) = mpsc::channel(16);
        let outgoing = Arc::new(OutgoingMessageSender::new(
            outgoing_tx,
            codex_analytics::AnalyticsEventsClient::disabled(),
        ));
        let processor = WorkflowRequestProcessor::new(
            Arc::new(config),
            config_manager,
            Arc::clone(&outgoing),
            /*workflow_app_server_url*/ None,
        );

        let Some(ClientResponsePayload::WorkflowRepair(response)) = processor
            .repair(WorkflowRepairParams {
                id: "broken/fix".to_string(),
                stage_session_id: None,
            })
            .await
            .unwrap()
        else {
            panic!("expected workflow repair response");
        };

        assert!(response.message.contains("Repairing workflow"));
        assert!(response.message.contains("Validation passed."));
        assert_eq!(
            response.repair.stop_reason,
            codex_app_server_protocol::WorkflowRepairStopReason::Valid
        );
        assert_eq!(response.repair.changed, true);
        assert!(!response.repair.applied_fixes.is_empty());
        assert_eq!(response.validation_command_results.len(), 2);
        assert!(
            response
                .validation_command_results
                .iter()
                .all(|result| result.succeeded)
        );

        let mut progress_messages = Vec::new();
        while let Ok(envelope) = outgoing_rx.try_recv() {
            let crate::outgoing_message::OutgoingEnvelope::Broadcast { message } = envelope else {
                continue;
            };
            let crate::outgoing_message::OutgoingMessage::AppServerNotification(
                codex_app_server_protocol::ServerNotification::WorkflowRunProgress(notification),
            ) = message
            else {
                continue;
            };
            progress_messages.push(notification.message);
        }
        assert!(
            progress_messages
                .iter()
                .any(|message| message == "Validating workflow")
        );
        assert!(
            progress_messages
                .iter()
                .any(|message| message == "Repair cycle started")
        );
        assert!(
            progress_messages
                .iter()
                .any(|message| message == "Workflow repair complete")
        );
    }
}
