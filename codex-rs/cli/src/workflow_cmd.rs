use anyhow::Result;
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_features::Feature;
use codex_utils_cli::CliConfigOverrides;
use codex_workflows::WORKFLOW_RUNTIME_EVENT_PREFIX;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowCommandProgress;
use codex_workflows::WorkflowSummary;
use codex_workflows::discover_workflows_for_context;
use codex_workflows::execute_workflow_command;
use codex_workflows::parse_workflow_command_with_workflows;
use serde_json::Value as JsonValue;
use serde_json::json;
use std::path::PathBuf;

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
    cwd: Option<PathBuf>,
    arg0_paths: Arg0DispatchPaths,
    stage_session_id: Option<&str>,
) -> Result<(Config, Vec<WorkflowSummary>)> {
    let cli_overrides = root_config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config_overrides = ConfigOverrides {
        config_profile,
        cwd,
        codex_self_exe: arg0_paths.codex_self_exe.clone(),
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
        ..Default::default()
    };
    let config =
        Config::load_with_cli_overrides_and_harness_overrides(cli_overrides, config_overrides)
            .await?;
    if config.features.enabled(Feature::Workflows) {
        codex_workflows::prefetch_managed_bun_runtime(config.codex_home.as_path());
    }
    let workflows = discover_workflows_for_context(&WorkflowCommandContext {
        codex_home: config.codex_home.as_path(),
        cwd: config.cwd.as_path(),
        config: &config.workflows,
        codex_self_exe: config.codex_self_exe.clone(),
        stage_session_id: stage_session_id.map(ToString::to_string),
        progress: None,
        runtime_event_handler: None,
        runtime: Default::default(),
    })?;
    Ok((config, workflows))
}

pub async fn run_workflow_command(
    cmd: WorkflowCli,
    root_config_overrides: CliConfigOverrides,
    config_profile: Option<String>,
    cwd: Option<PathBuf>,
    arg0_paths: Arg0DispatchPaths,
) -> Result<()> {
    let (config, workflows) = load_workflow_command_context(
        root_config_overrides,
        config_profile,
        cwd,
        arg0_paths,
        cmd.stage_session_id.as_deref(),
    )
    .await?;
    let command = parse_workflow_command_with_workflows(&cmd.args, &workflows)?;
    let workflow_run_id = std::env::var("CODEX_WORKFLOW_RUN_ID").ok();
    let progress = |event: WorkflowCommandProgress| {
        if workflow_run_id.is_some() {
            let payload = json!({
                "type": "progress",
                "message": event.message,
                "data": event.data,
            });
            eprintln!("{WORKFLOW_RUNTIME_EVENT_PREFIX}{payload}");
        } else {
            let detail = event.data.as_ref().and_then(format_cli_progress_data);
            match (event.message.is_empty(), detail) {
                (true, Some(detail)) => eprintln!("{detail}"),
                (false, Some(detail)) => eprintln!("{} ({detail})", event.message),
                (false, None) => eprintln!("{}", event.message),
                (true, None) => {}
            }
        }
    };
    let output = execute_workflow_command(
        WorkflowCommandContext {
            codex_home: config.codex_home.as_path(),
            cwd: config.cwd.as_path(),
            config: &config.workflows,
            codex_self_exe: config.codex_self_exe.clone(),
            stage_session_id: cmd.stage_session_id,
            progress: Some(&progress),
            runtime_event_handler: None,
            runtime: Default::default(),
        },
        command,
    )?;
    println!("{}", output.message);
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}

fn format_cli_progress_data(data: &JsonValue) -> Option<String> {
    let object = data.as_object()?;
    let mut parts = Vec::new();

    if let Some(workflow_id) = object.get("workflowId").and_then(simple_progress_value) {
        parts.push(workflow_id);
    }

    match (
        object.get("step").and_then(simple_progress_value),
        object.get("total").and_then(simple_progress_value),
    ) {
        (Some(step), Some(total)) => parts.push(format!("cycle {step}/{total}")),
        (Some(step), None) => parts.push(format!("cycle {step}")),
        (None, Some(total)) => parts.push(format!("total cycles {total}")),
        (None, None) => {}
    }

    for key in [
        "mode",
        "maxRepairCycles",
        "findings",
        "fixes",
        "stopReason",
        "changed",
    ] {
        if let Some(value) = object.get(key).and_then(simple_progress_value) {
            parts.push(match key {
                "maxRepairCycles" => format!("max cycles {value}"),
                "stopReason" => format!("stop reason {value}"),
                _ => format!("{key} {value}"),
            });
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn simple_progress_value(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::Null => None,
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Number(number) => Some(number.to_string()),
        JsonValue::Bool(flag) => Some(flag.to_string()),
        JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::format_cli_progress_data;

    #[test]
    fn cli_progress_data_formats_repair_context_without_raw_json() {
        let data = json!({
            "stage": "repairing",
            "workflowId": "broken/fix",
            "step": 2,
            "total": 3,
            "findings": 4,
            "fixes": 2,
        });

        assert_eq!(
            format_cli_progress_data(&data),
            Some("broken/fix, cycle 2/3, findings 4, fixes 2".to_string())
        );
    }
}
