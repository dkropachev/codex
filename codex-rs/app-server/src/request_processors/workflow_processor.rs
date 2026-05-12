use std::fs;
use std::sync::Arc;

use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::WorkflowAuthoringContextPrepareParams;
use codex_app_server_protocol::WorkflowAuthoringContextPrepareResponse;
use codex_app_server_protocol::WorkflowCommandExecuteParams;
use codex_app_server_protocol::WorkflowCommandExecuteResponse;
use codex_app_server_protocol::WorkflowCommandResponse;
use codex_app_server_protocol::WorkflowConfigReadParams;
use codex_app_server_protocol::WorkflowConfigReadResponse;
use codex_app_server_protocol::WorkflowConfigValues;
use codex_app_server_protocol::WorkflowConfigWriteParams;
use codex_app_server_protocol::WorkflowConfigWriteResponse;
use codex_app_server_protocol::WorkflowDevelopParams;
use codex_app_server_protocol::WorkflowDevelopResponse;
use codex_app_server_protocol::WorkflowEditParams;
use codex_app_server_protocol::WorkflowEditResponse;
use codex_app_server_protocol::WorkflowImpactInfo;
use codex_app_server_protocol::WorkflowImpactParams;
use codex_app_server_protocol::WorkflowImpactResponse;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowReadParams;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRepairParams;
use codex_app_server_protocol::WorkflowRepairResponse;
use codex_app_server_protocol::WorkflowRootInfo;
use codex_app_server_protocol::WorkflowRootKind;
use codex_app_server_protocol::WorkflowRunParams;
use codex_app_server_protocol::WorkflowRunResponse;
use codex_app_server_protocol::WorkflowSummary;
use codex_app_server_protocol::WorkflowValidateParams;
use codex_app_server_protocol::WorkflowValidateResponse;
use codex_app_server_protocol::WorkflowValidationInfo;
use codex_app_server_protocol::WorkflowValidationStatus;
use codex_config::types::WorkflowDefaultLocation;
use codex_config::types::WorkflowsConfigToml;
use codex_core::config::Config;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowConfigCommand;
use codex_workflows::WorkflowInputSource;
use codex_workflows::discover_workflows;
use codex_workflows::execute_workflow_command;
use codex_workflows::find_workflow;
use codex_workflows::parse_mention_target;
use codex_workflows::parse_workflow_command;
use codex_workflows::workflow_impact;
use serde_json::Value as JsonValue;

use crate::config_manager::ConfigManager;
use crate::error_code::internal_error;
use crate::error_code::invalid_params;

#[derive(Clone)]
pub(crate) struct WorkflowRequestProcessor {
    config: Arc<Config>,
    config_manager: ConfigManager,
}

impl WorkflowRequestProcessor {
    pub(crate) fn new(config: Arc<Config>, config_manager: ConfigManager) -> Self {
        Self {
            config,
            config_manager,
        }
    }

    pub(crate) async fn list(
        &self,
        _params: WorkflowListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let roots = codex_workflows::workflow_roots(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
        )
        .into_iter()
        .map(root_to_api)
        .collect();
        let workflows = self.discover_api_workflows()?;
        Ok(Some(WorkflowListResponse { roots, workflows }.into()))
    }

    pub(crate) async fn read(
        &self,
        params: WorkflowReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let workflow = self.resolve_workflow(params.id, params.target)?;
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
        let workflow = self.resolve_workflow(params.id, None)?;
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
        self.execute(WorkflowCommand::Develop {
            description: params.description,
        })
        .map(|response: WorkflowDevelopResponse| Some(response.into()))
    }

    pub(crate) async fn edit(
        &self,
        params: WorkflowEditParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(WorkflowCommand::Edit {
            id: params.id,
            instruction: params.instruction,
        })
        .map(|response: WorkflowEditResponse| Some(response.into()))
    }

    pub(crate) async fn run(
        &self,
        params: WorkflowRunParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let input = params
            .input
            .map(|value| WorkflowInputSource::Inline(value.to_string()));
        self.execute(WorkflowCommand::Run {
            id: params.id,
            input,
        })
        .map(|response: WorkflowRunResponse| Some(response.into()))
    }

    pub(crate) async fn validate(
        &self,
        params: WorkflowValidateParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(WorkflowCommand::Validate { id: params.id })
            .map(|response: WorkflowValidateResponse| Some(response.into()))
    }

    pub(crate) async fn repair(
        &self,
        params: WorkflowRepairParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.execute(WorkflowCommand::Fix { id: params.id })
            .map(|response: WorkflowRepairResponse| Some(response.into()))
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
        let _response = self.execute::<WorkflowCommandResponse>(command)?;
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
        let command = parse_workflow_command(&params.args)
            .map_err(|err| invalid_params(format!("invalid workflow command: {err}")))?;
        self.execute(command)
            .map(|response: WorkflowCommandExecuteResponse| Some(response.into()))
    }

    pub(crate) async fn authoring_context_prepare(
        &self,
        _params: WorkflowAuthoringContextPrepareParams,
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
                workflows: self.discover_api_workflows()?,
                config: config_values(&self.config.workflows),
            }
            .into(),
        ))
    }

    fn discover_api_workflows(&self) -> Result<Vec<WorkflowSummary>, JSONRPCErrorError> {
        discover_workflows(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
        )
        .map(|workflows| workflows.into_iter().map(summary_to_api).collect())
        .map_err(|err| internal_error(format!("failed to discover workflows: {err}")))
    }

    fn resolve_workflow(
        &self,
        id: String,
        target: Option<String>,
    ) -> Result<WorkflowSummary, JSONRPCErrorError> {
        if let Some(target) = target {
            let parsed = parse_mention_target(&target)
                .map_err(|err| invalid_params(format!("invalid workflow target: {err}")))?;
            return self
                .discover_api_workflows()?
                .into_iter()
                .find(|workflow| workflow.id == parsed.id && workflow.root_path == parsed.root_path)
                .ok_or_else(|| invalid_params("workflow target was not found"));
        }

        find_workflow(
            self.config.codex_home.as_path(),
            self.config.cwd.as_path(),
            &self.config.workflows,
            &id,
        )
        .map(summary_to_api)
        .map_err(|err| invalid_params(format!("failed to resolve workflow: {err}")))
    }

    fn execute<T>(&self, command: WorkflowCommand) -> Result<T, JSONRPCErrorError>
    where
        T: From<WorkflowCommandResponse>,
    {
        execute_workflow_command(
            WorkflowCommandContext {
                codex_home: self.config.codex_home.as_path(),
                cwd: self.config.cwd.as_path(),
                config: &self.config.workflows,
            },
            command,
        )
        .map(|output| WorkflowCommandResponse {
            message: output.message,
            data: output.data,
        })
        .map(T::from)
        .map_err(|err| internal_error(format!("workflow command failed: {err}")))
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
        title: summary.title,
        user_description: summary.user_description,
        search_terms: summary.search_terms,
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
        title: summary.title.clone(),
        user_description: summary.user_description.clone(),
        search_terms: summary.search_terms.clone(),
        root_label: summary.root_label.clone(),
        root_kind: root_kind_from_api(summary.root_kind),
        root_path: summary.root_path.clone(),
        path: summary.path.clone(),
        workflow_yaml_path: summary.workflow_yaml_path.clone(),
        mention_target: summary.mention_target.clone(),
        validation: codex_workflows::WorkflowValidation {
            status: match summary.validation.status {
                WorkflowValidationStatus::Valid => codex_workflows::WorkflowValidationStatus::Valid,
                WorkflowValidationStatus::Invalid => {
                    codex_workflows::WorkflowValidationStatus::Invalid
                }
            },
            messages: summary.validation.messages.clone(),
        },
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

fn validation_to_api(validation: codex_workflows::WorkflowValidation) -> WorkflowValidationInfo {
    WorkflowValidationInfo {
        status: match validation.status {
            codex_workflows::WorkflowValidationStatus::Valid => WorkflowValidationStatus::Valid,
            codex_workflows::WorkflowValidationStatus::Invalid => WorkflowValidationStatus::Invalid,
        },
        messages: validation.messages,
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
