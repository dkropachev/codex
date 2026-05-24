use anyhow::Result;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_utils_cli::CliConfigOverrides;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowSummary;
use codex_workflows::discover_workflows_for_context;
use codex_workflows::execute_workflow_command;
use codex_workflows::parse_workflow_command_with_workflows;

#[derive(Debug, clap::Parser)]
#[command(bin_name = "codex workflow")]
pub struct WorkflowCli {
    #[arg(long, hide = true)]
    pub stage_session_id: Option<String>,

    /// Workflow command and arguments, such as `list`, `validate <id>`, `repair <id>` / `fix <id>`, `run <id> --input '{...}'`, `run <id> --findings confirmed|filtered|both`, or a registered alias.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub(crate) async fn load_workflow_command_context(
    root_config_overrides: CliConfigOverrides,
    config_profile: Option<String>,
    arg0_paths: Arg0DispatchPaths,
    stage_session_id: Option<&str>,
) -> Result<(Config, Vec<WorkflowSummary>)> {
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
    let workflows = discover_workflows_for_context(&WorkflowCommandContext {
        codex_home: config.codex_home.as_path(),
        cwd: config.cwd.as_path(),
        config: &config.workflows,
        codex_self_exe: config.codex_self_exe.clone(),
        stage_session_id: stage_session_id.map(ToString::to_string),
    })?;
    Ok((config, workflows))
}

pub async fn run_workflow_command(
    cmd: WorkflowCli,
    root_config_overrides: CliConfigOverrides,
    config_profile: Option<String>,
    arg0_paths: Arg0DispatchPaths,
) -> Result<()> {
    let (config, workflows) = load_workflow_command_context(
        root_config_overrides,
        config_profile,
        arg0_paths,
        cmd.stage_session_id.as_deref(),
    )
    .await?;
    let command = parse_workflow_command_with_workflows(&cmd.args, &workflows)?;
    let output = execute_workflow_command(
        WorkflowCommandContext {
            codex_home: config.codex_home.as_path(),
            cwd: config.cwd.as_path(),
            config: &config.workflows,
            codex_self_exe: config.codex_self_exe.clone(),
            stage_session_id: cmd.stage_session_id,
        },
        command,
    )?;
    println!("{}", output.message);
    Ok(())
}
