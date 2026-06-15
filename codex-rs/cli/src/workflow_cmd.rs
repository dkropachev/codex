use std::process::Command;

use anyhow::Context;
use anyhow::bail;
use clap::Parser;
use codex_core::config::Config;
use codex_features::Feature;
use codex_tui::workflow_commands::WorkflowCommand;
use codex_tui::workflow_commands::discover_workflow_commands;
use codex_tui::workflow_commands::workflow_invocation_input_from_args;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(bin_name = "codex workflow")]
pub struct WorkflowCli {
    #[command(subcommand)]
    pub subcommand: WorkflowSubcommand,
}

#[derive(Debug, clap::Subcommand)]
pub enum WorkflowSubcommand {
    /// List configured workflow commands.
    List(WorkflowListArgs),

    /// Run a configured workflow command.
    Run(WorkflowRunArgs),
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex workflow list")]
pub struct WorkflowListArgs {
    /// Output workflow commands as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
#[command(
    bin_name = "codex workflow run",
    override_usage = "codex workflow run <COMMAND> [-- <ARGS>...]"
)]
pub struct WorkflowRunArgs {
    /// Workflow command name from workflow.yaml.
    command: String,

    /// Arguments forwarded into the workflow input JSON.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonWorkflowCommand {
    command: String,
    description: String,
    workflow_dir: String,
}

impl From<&WorkflowCommand> for JsonWorkflowCommand {
    fn from(command: &WorkflowCommand) -> Self {
        Self {
            command: command.command.clone(),
            description: command.description.clone(),
            workflow_dir: command.workflow_dir.display().to_string(),
        }
    }
}

pub fn run(cli: WorkflowCli, config: &Config) -> anyhow::Result<()> {
    ensure_workflows_enabled(config)?;
    let commands = discover_workflow_commands(config.codex_home.as_path(), config.cwd.as_path());
    match cli.subcommand {
        WorkflowSubcommand::List(args) => run_list(args, &commands),
        WorkflowSubcommand::Run(args) => run_workflow(args, config, &commands),
    }
}

fn ensure_workflows_enabled(config: &Config) -> anyhow::Result<()> {
    if config.features.enabled(Feature::Workflows) {
        return Ok(());
    }

    bail!(
        "`codex workflow` requires the `workflows` feature. Enable it with `codex features enable workflows` or pass `--enable workflows`."
    )
}

fn run_list(args: WorkflowListArgs, commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    if args.json {
        let output = commands
            .iter()
            .map(JsonWorkflowCommand::from)
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    if commands.is_empty() {
        println!("No workflow commands found.");
        return Ok(());
    }

    let command_width = commands
        .iter()
        .map(|command| command.command.len())
        .max()
        .unwrap_or(0);
    for command in commands {
        println!(
            "{:<command_width$}  {}  {}",
            command.command,
            command.description,
            command.workflow_dir.display()
        );
    }

    Ok(())
}

fn run_workflow(
    args: WorkflowRunArgs,
    config: &Config,
    commands: &[WorkflowCommand],
) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, &args.command)?;
    let input = workflow_invocation_input_from_args(config.cwd.as_path(), &args.args)
        .map_err(|err| anyhow::anyhow!("{}", err.message()))?;
    let input_json = serde_json::to_string(&input).context("failed to serialize workflow input")?;
    let status = Command::new("bun")
        .arg("src/workflow.ts")
        .arg("--input")
        .arg(input_json)
        .current_dir(&command.workflow_dir)
        .status()
        .with_context(|| {
            format!(
                "failed to run workflow command `{}` with `bun`",
                command.command
            )
        })?;

    if status.success() {
        return Ok(());
    }

    std::process::exit(status.code().unwrap_or(1));
}

fn find_workflow_command<'a>(
    commands: &'a [WorkflowCommand],
    command: &str,
) -> anyhow::Result<&'a WorkflowCommand> {
    if let Some(workflow_command) = commands
        .iter()
        .find(|workflow_command| workflow_command.command == command)
    {
        return Ok(workflow_command);
    }

    let available = commands
        .iter()
        .map(|workflow_command| workflow_command.command.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        bail!("Unknown workflow command `{command}`. No workflow commands found.");
    }

    bail!("Unknown workflow command `{command}`. Available commands: {available}.");
}
