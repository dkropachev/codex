use anyhow::Result;
use codex_arg0::Arg0DispatchPaths;
use codex_utils_cli::CliConfigOverrides;
use codex_workflows::WorkflowCommand;
use codex_workflows::WorkflowCommandContext;
use codex_workflows::WorkflowEngine;
use codex_workflows::WorkflowInputSource;
use codex_workflows::WorkflowRuntimeContext;
use codex_workflows::execute_workflow_command;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::workflow_cmd::load_workflow_command_context;
use crate::workflow_cmd::native_model_candidates_from_config;

#[derive(Debug, clap::Parser)]
#[command(bin_name = "codex native-workflow")]
pub struct NativeWorkflowCli {
    #[command(subcommand)]
    pub command: NativeWorkflowSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum NativeWorkflowSubcommand {
    /// List compiled-in native workflows.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Run a compiled-in native workflow.
    Run {
        id: String,
        #[arg(long)]
        input: Option<String>,
    },
}

pub async fn run_native_workflow_command(
    cmd: NativeWorkflowCli,
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
        /*stage_session_id*/ None,
    )
    .await?;
    let native_workflows = workflows
        .iter()
        .filter(|workflow| workflow.engine == WorkflowEngine::Rust)
        .cloned()
        .collect::<Vec<_>>();

    match cmd.command {
        NativeWorkflowSubcommand::List { json: json_output } => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&native_workflows)?);
            } else if native_workflows.is_empty() {
                println!("No native workflows found.");
            } else {
                for workflow in native_workflows {
                    let command = workflow.command.as_deref().unwrap_or("-");
                    let title = workflow.title.as_deref().unwrap_or("untitled");
                    println!("{}\t{}\t{}", workflow.id, command, title);
                }
            }
        }
        NativeWorkflowSubcommand::Run { id, input } => {
            let Some(workflow) = native_workflows.iter().find(|workflow| workflow.id == id) else {
                anyhow::bail!("native workflow '{id}' was not found or is disabled");
            };
            let output = execute_workflow_command(
                WorkflowCommandContext {
                    codex_home: config.codex_home.as_path(),
                    cwd: config.cwd.as_path(),
                    config: &config.workflows,
                    codex_self_exe: config.codex_self_exe.clone(),
                    stage_session_id: None,
                    progress: None,
                    runtime_event_handler: None,
                    runtime: WorkflowRuntimeContext {
                        model_candidates: native_model_candidates_from_config(&config),
                        ..Default::default()
                    },
                },
                WorkflowCommand::Run {
                    id: workflow.id.clone(),
                    input: input.map(WorkflowInputSource::Inline),
                    input_fields: BTreeMap::new(),
                },
            )?;
            println!("{}", output.message);
            if output.exit_code != 0 {
                std::process::exit(output.exit_code);
            }
        }
    }
    Ok(())
}
