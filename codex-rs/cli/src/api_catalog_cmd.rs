use std::collections::BTreeSet;

use anyhow::Result;
use clap::ValueEnum;
use codex_app_server_protocol::ApiCatalogMethod;
use codex_app_server_protocol::ApiCatalogReadResponse;
use codex_app_server_protocol::McpServerStatus;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_core_plugins::PluginsManager;
use codex_exec_server::EnvironmentManager;
use codex_exec_server::EnvironmentManagerArgs;
use codex_exec_server::ExecServerRuntimePaths;
use codex_login::AuthManager;
use codex_mcp::McpRuntimeEnvironment;
use codex_mcp::McpServerStatusSnapshot;
use codex_mcp::McpSnapshotDetail;
use codex_mcp::collect_mcp_server_status_snapshot_with_detail;
use codex_mcp::effective_mcp_servers;
use codex_protocol::protocol::McpAuthStatus as CoreMcpAuthStatus;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, clap::Parser)]
pub struct ApiCatalogCli {
    /// Controls how much MCP inventory data to fetch.
    #[arg(long = "mcp-detail", value_enum, default_value_t = ApiCatalogMcpDetail::Full)]
    pub mcp_detail: ApiCatalogMcpDetail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApiCatalogMcpDetail {
    Full,
    ToolsAndAuthOnly,
}

impl From<ApiCatalogMcpDetail> for McpSnapshotDetail {
    fn from(value: ApiCatalogMcpDetail) -> Self {
        match value {
            ApiCatalogMcpDetail::Full => Self::Full,
            ApiCatalogMcpDetail::ToolsAndAuthOnly => Self::ToolsAndAuthOnly,
        }
    }
}

pub async fn run_api_catalog_command(
    cmd: ApiCatalogCli,
    root_config_overrides: CliConfigOverrides,
    config_profile: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> Result<()> {
    let cli_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config_overrides = ConfigOverrides {
        config_profile,
        codex_self_exe: arg0_paths.codex_self_exe.clone(),
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
        ..Default::default()
    };
    let config =
        Config::load_with_cli_overrides_and_harness_overrides(cli_overrides, config_overrides)
            .await?;
    let response = build_api_catalog_response(cmd, config, arg0_paths).await?;
    serde_json::to_writer_pretty(std::io::stdout(), &response)?;
    println!();
    Ok(())
}

async fn build_api_catalog_response(
    cmd: ApiCatalogCli,
    config: Config,
    arg0_paths: Arg0DispatchPaths,
) -> Result<ApiCatalogReadResponse> {
    let mcp_servers = api_catalog_mcp_servers(&config, cmd.mcp_detail, arg0_paths).await?;
    Ok(ApiCatalogReadResponse {
        schema_version: 1,
        generated_at: chrono::Utc::now().timestamp(),
        app_server_methods: api_catalog_methods(),
        mcp_servers,
        built_in_tools: codex_app_server_protocol::built_in_api_catalog_tools(),
        workflow_runtime: codex_app_server_protocol::workflow_runtime_api_catalog(),
        workflows: api_catalog_workflows(&config)?,
    })
}

fn api_catalog_workflows(
    config: &Config,
) -> Result<Vec<codex_app_server_protocol::WorkflowSummary>> {
    Ok(codex_workflows::discover_workflows(
        config.codex_home.as_path(),
        config.cwd.as_path(),
        &config.workflows,
    )?
    .into_iter()
    .map(api_catalog_workflow_to_info)
    .collect())
}

async fn api_catalog_mcp_servers(
    config: &Config,
    detail: ApiCatalogMcpDetail,
    arg0_paths: Arg0DispatchPaths,
) -> Result<Vec<McpServerStatus>> {
    let plugins_manager = PluginsManager::new(config.codex_home.to_path_buf());
    let mcp_config = config.to_mcp_config(&plugins_manager).await;
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ true);
    let auth = auth_manager.auth().await;
    let runtime_paths = ExecServerRuntimePaths::from_optional_paths(
        arg0_paths.codex_self_exe,
        arg0_paths.codex_linux_sandbox_exe,
    )?;
    let environment_manager =
        EnvironmentManager::new(EnvironmentManagerArgs::new(runtime_paths)).await;
    let environment = environment_manager
        .default_environment()
        .unwrap_or_else(|| environment_manager.local_environment());
    let runtime_environment = McpRuntimeEnvironment::new(environment, config.cwd.to_path_buf());
    let snapshot = collect_mcp_server_status_snapshot_with_detail(
        &mcp_config,
        auth.as_ref(),
        "codex api".to_string(),
        runtime_environment,
        detail.into(),
    )
    .await;
    let effective_servers = effective_mcp_servers(&mcp_config, auth.as_ref());
    let McpServerStatusSnapshot {
        tools_by_server,
        resources,
        resource_templates,
        auth_statuses,
    } = snapshot;
    let mut server_names: BTreeSet<String> =
        mcp_config.configured_mcp_servers.keys().cloned().collect();
    server_names.extend(effective_servers.keys().cloned());
    server_names.extend(auth_statuses.keys().cloned());
    server_names.extend(resources.keys().cloned());
    server_names.extend(resource_templates.keys().cloned());

    Ok(server_names
        .into_iter()
        .map(|name| McpServerStatus {
            name: name.clone(),
            tools: tools_by_server.get(&name).cloned().unwrap_or_default(),
            resources: resources.get(&name).cloned().unwrap_or_default(),
            resource_templates: resource_templates.get(&name).cloned().unwrap_or_default(),
            auth_status: auth_statuses
                .get(&name)
                .cloned()
                .unwrap_or(CoreMcpAuthStatus::Unsupported)
                .into(),
        })
        .collect())
}

fn api_catalog_methods() -> Vec<ApiCatalogMethod> {
    codex_app_server_protocol::client_method_infos()
        .into_iter()
        .map(|method| ApiCatalogMethod {
            method: method.method,
            params_type: method.params_type.to_string(),
            response_type: method.response_type.to_string(),
            experimental: method.experimental,
            description: method.description,
        })
        .collect()
}

fn api_catalog_workflow_to_info(
    workflow: codex_workflows::WorkflowSummary,
) -> codex_app_server_protocol::WorkflowSummary {
    codex_app_server_protocol::WorkflowSummary {
        id: workflow.id,
        title: workflow.title,
        user_description: workflow.user_description,
        search_terms: workflow.search_terms,
        root_label: workflow.root_label,
        root_kind: match workflow.root_kind {
            codex_workflows::WorkflowRootKind::Global => {
                codex_app_server_protocol::WorkflowRootKind::Global
            }
            codex_workflows::WorkflowRootKind::Project => {
                codex_app_server_protocol::WorkflowRootKind::Project
            }
            codex_workflows::WorkflowRootKind::SearchPath => {
                codex_app_server_protocol::WorkflowRootKind::SearchPath
            }
        },
        root_path: workflow.root_path,
        path: workflow.path,
        workflow_yaml_path: workflow.workflow_yaml_path,
        mention_target: workflow.mention_target,
        validation: codex_app_server_protocol::WorkflowValidationInfo {
            status: match workflow.validation.status {
                codex_workflows::WorkflowValidationStatus::Valid => {
                    codex_app_server_protocol::WorkflowValidationStatus::Valid
                }
                codex_workflows::WorkflowValidationStatus::Invalid => {
                    codex_app_server_protocol::WorkflowValidationStatus::Invalid
                }
            },
            messages: workflow.validation.messages,
        },
        repair_mode: workflow.repair_mode,
    }
}
