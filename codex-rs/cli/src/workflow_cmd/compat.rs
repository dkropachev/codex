use std::fs;
use std::path::Path;
use std::path::PathBuf;
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
use serde_json::Value;
use serde_json::json;

const DEFAULT_MAX_REPAIR_CYCLES: u32 = 3;

#[derive(Debug, Parser)]
#[command(bin_name = "codex workflow")]
pub struct WorkflowCli {
    #[arg(long, hide = true)]
    pub stage_session_id: Option<String>,

    /// Workflow command and arguments, such as `list`, `validate <id>`,
    /// `repair <id>` / `fix <id>`, `recover <id>`, `run <id> --input '{...}'`,
    /// or a registered workflow alias.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug)]
enum ParsedWorkflowCommand {
    Mode,
    List {
        json: bool,
    },
    Run {
        target: String,
        args: Vec<String>,
        invocation: WorkflowInvocationKind,
    },
    Fix {
        target: String,
        args: Vec<String>,
    },
    Recover {
        target: String,
        args: Vec<String>,
    },
    Validate {
        target: String,
    },
    Impact {
        target: String,
    },
    Status {
        target: Option<String>,
    },
    Show {
        target: String,
        json: bool,
    },
    Where {
        target: String,
    },
    Config(WorkflowConfigCommand),
    Develop(WorkflowDevelopRequest),
    Describe {
        target: String,
        description: String,
    },
    Docs {
        target: String,
        instruction: String,
    },
    Edit {
        target: String,
        instruction: String,
    },
    Publish,
    Discard,
    Done,
}

#[derive(Debug)]
enum WorkflowConfigCommand {
    Show,
    Set { key: String, value: String },
    Clear { key: String },
}

#[derive(Debug, Clone, Copy)]
enum WorkflowInvocationKind {
    Explicit,
    Alias,
}

#[derive(Debug)]
struct WorkflowDevelopRequest {
    description: String,
    id: Option<String>,
    command: Option<String>,
    title: Option<String>,
    location: Option<WorkflowDevelopLocation>,
}

#[derive(Debug, Clone, Copy)]
enum WorkflowDevelopLocation {
    Project,
    Global,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonWorkflowCommand {
    id: String,
    command: String,
    description: String,
    workflow_dir: String,
}

impl From<&WorkflowCommand> for JsonWorkflowCommand {
    fn from(command: &WorkflowCommand) -> Self {
        Self {
            id: command.id.clone(),
            command: command.command.clone(),
            description: command.description.clone(),
            workflow_dir: command.workflow_dir.display().to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkflowAction {
    Run,
    Fix,
    Recover,
}

impl WorkflowAction {
    fn input_action(self) -> Option<&'static str> {
        match self {
            WorkflowAction::Run => None,
            WorkflowAction::Fix => Some("fix"),
            WorkflowAction::Recover => Some("recover"),
        }
    }
}

pub fn run(cli: WorkflowCli, config: &Config) -> anyhow::Result<()> {
    ensure_workflows_enabled(config)?;
    let commands = discover_workflow_commands(config.codex_home.as_path(), config.cwd.as_path());
    let command = parse_workflow_command(&cli.args, &commands)?;
    match command {
        ParsedWorkflowCommand::Mode => show_mode(&commands),
        ParsedWorkflowCommand::List { json } => list_workflows(&commands, json),
        ParsedWorkflowCommand::Run {
            target,
            args,
            invocation,
        } => run_workflow(
            target,
            args,
            invocation,
            WorkflowAction::Run,
            config,
            &commands,
        ),
        ParsedWorkflowCommand::Fix { target, args } => run_workflow(
            target,
            args,
            WorkflowInvocationKind::Explicit,
            WorkflowAction::Fix,
            config,
            &commands,
        ),
        ParsedWorkflowCommand::Recover { target, args } => run_workflow(
            target,
            args,
            WorkflowInvocationKind::Explicit,
            WorkflowAction::Recover,
            config,
            &commands,
        ),
        ParsedWorkflowCommand::Validate { target } => validate_workflow(&target, &commands),
        ParsedWorkflowCommand::Impact { target } => impact_workflow(&target, &commands),
        ParsedWorkflowCommand::Status { target } => status_workflows(target.as_deref(), &commands),
        ParsedWorkflowCommand::Show { target, json } => show_workflow(&target, &commands, json),
        ParsedWorkflowCommand::Where { target } => where_workflow(&target, &commands),
        ParsedWorkflowCommand::Config(command) => run_config_command(command),
        ParsedWorkflowCommand::Develop(request) => develop_workflow(request, config),
        ParsedWorkflowCommand::Describe {
            target,
            description,
        } => update_workflow_description(&target, &description, &commands),
        ParsedWorkflowCommand::Docs {
            target,
            instruction,
        } => append_workflow_note(&target, "Documentation", &instruction, &commands),
        ParsedWorkflowCommand::Edit {
            target,
            instruction,
        } => append_workflow_note(&target, "Edit request", &instruction, &commands),
        ParsedWorkflowCommand::Publish => publish_workflows(cli.stage_session_id.as_deref()),
        ParsedWorkflowCommand::Discard => discard_workflows(cli.stage_session_id.as_deref()),
        ParsedWorkflowCommand::Done => done_workflow_mode(cli.stage_session_id.as_deref()),
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

fn parse_workflow_command(
    args: &[String],
    workflows: &[WorkflowCommand],
) -> anyhow::Result<ParsedWorkflowCommand> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(ParsedWorkflowCommand::Mode);
    };
    match command {
        "develop" => parse_develop(args),
        "describe" => Ok(ParsedWorkflowCommand::Describe {
            target: required(args, /*index*/ 1, "describe", "a workflow id")?.to_string(),
            description: joined_tail(args, /*start*/ 2, "describe", "a description")?,
        }),
        "docs" => Ok(ParsedWorkflowCommand::Docs {
            target: required(args, /*index*/ 1, "docs", "a workflow id")?.to_string(),
            instruction: joined_tail(args, /*start*/ 2, "docs", "an instruction")?,
        }),
        "edit" => Ok(ParsedWorkflowCommand::Edit {
            target: required(args, /*index*/ 1, "edit", "a workflow id")?.to_string(),
            instruction: joined_tail(args, /*start*/ 2, "edit", "an instruction")?,
        }),
        "repair" | "fix" => Ok(ParsedWorkflowCommand::Fix {
            target: required(args, /*index*/ 1, command, "a workflow id")?.to_string(),
            args: args.get(2..).unwrap_or_default().to_vec(),
        }),
        "recover" => Ok(ParsedWorkflowCommand::Recover {
            target: required(args, /*index*/ 1, "recover", "a workflow id")?.to_string(),
            args: args.get(2..).unwrap_or_default().to_vec(),
        }),
        "run" => Ok(ParsedWorkflowCommand::Run {
            target: required(args, /*index*/ 1, "run", "a workflow id")?.to_string(),
            args: args.get(2..).unwrap_or_default().to_vec(),
            invocation: WorkflowInvocationKind::Explicit,
        }),
        "validate" => Ok(ParsedWorkflowCommand::Validate {
            target: single_id(args, "validate")?,
        }),
        "impact" => Ok(ParsedWorkflowCommand::Impact {
            target: single_id(args, "impact")?,
        }),
        "status" => {
            if args.len() > 2 {
                bail!("unexpected argument '{}'", args[2]);
            }
            Ok(ParsedWorkflowCommand::Status {
                target: args.get(1).cloned(),
            })
        }
        "list" => parse_list(args),
        "show" => parse_show(args),
        "where" => Ok(ParsedWorkflowCommand::Where {
            target: single_id(args, "where")?,
        }),
        "config" => parse_config(args),
        "publish" => {
            expect_no_extra(args)?;
            Ok(ParsedWorkflowCommand::Publish)
        }
        "discard" => {
            expect_no_extra(args)?;
            Ok(ParsedWorkflowCommand::Discard)
        }
        "done" => {
            expect_no_extra(args)?;
            Ok(ParsedWorkflowCommand::Done)
        }
        alias => {
            if find_workflow_command(workflows, alias).is_ok() {
                Ok(ParsedWorkflowCommand::Run {
                    target: alias.to_string(),
                    args: args.get(1..).unwrap_or_default().to_vec(),
                    invocation: WorkflowInvocationKind::Alias,
                })
            } else {
                bail!("unknown workflow command `{alias}`");
            }
        }
    }
}

fn parse_develop(args: &[String]) -> anyhow::Result<ParsedWorkflowCommand> {
    let mut id = None;
    let mut command = None;
    let mut title = None;
    let mut location = None;
    let mut description_parts = Vec::new();
    let mut index = 1;

    while index < args.len() {
        match parse_long_flag_argument(&args[index]) {
            Some(("id", Some(value))) => {
                id = Some(value.to_string());
                index += 1;
            }
            Some(("id", None)) => {
                id = Some(required(args, index + 1, "develop", "a workflow id")?.to_string());
                index += 2;
            }
            Some(("command", Some(value))) => {
                command = Some(value.to_string());
                index += 1;
            }
            Some(("command", None)) => {
                command = Some(required(args, index + 1, "develop", "a command")?.to_string());
                index += 2;
            }
            Some(("title", Some(value))) => {
                title = Some(value.to_string());
                index += 1;
            }
            Some(("title", None)) => {
                title = Some(required(args, index + 1, "develop", "a title")?.to_string());
                index += 2;
            }
            Some(("location", Some(value))) => {
                location = Some(parse_develop_location(value)?);
                index += 1;
            }
            Some(("location", None)) => {
                location = Some(parse_develop_location(required(
                    args,
                    index + 1,
                    "develop",
                    "project or global",
                )?)?);
                index += 2;
            }
            Some((flag, _)) => bail!("unexpected argument '--{flag}'"),
            None => {
                description_parts.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    if description_parts.is_empty() {
        bail!("workflow command 'develop' requires a description");
    }

    Ok(ParsedWorkflowCommand::Develop(WorkflowDevelopRequest {
        description: description_parts.join(" "),
        id,
        command,
        title,
        location,
    }))
}

fn parse_develop_location(value: &str) -> anyhow::Result<WorkflowDevelopLocation> {
    match value {
        "project" => Ok(WorkflowDevelopLocation::Project),
        "global" => Ok(WorkflowDevelopLocation::Global),
        _ => bail!("unexpected argument '--location {value}'"),
    }
}

fn parse_list(args: &[String]) -> anyhow::Result<ParsedWorkflowCommand> {
    match args.get(1).map(String::as_str) {
        None => Ok(ParsedWorkflowCommand::List { json: false }),
        Some("--json") if args.len() == 2 => Ok(ParsedWorkflowCommand::List { json: true }),
        Some(extra) => bail!("unexpected argument '{extra}'"),
    }
}

fn parse_show(args: &[String]) -> anyhow::Result<ParsedWorkflowCommand> {
    let target = required(args, /*index*/ 1, "show", "a workflow id")?.to_string();
    match args.get(2).map(String::as_str) {
        None => Ok(ParsedWorkflowCommand::Show {
            target,
            json: false,
        }),
        Some("--json") if args.len() == 3 => Ok(ParsedWorkflowCommand::Show { target, json: true }),
        Some(extra) => bail!("unexpected argument '{extra}'"),
    }
}

fn parse_config(args: &[String]) -> anyhow::Result<ParsedWorkflowCommand> {
    match args.get(1).map(String::as_str) {
        Some("show") if args.len() == 2 => {
            Ok(ParsedWorkflowCommand::Config(WorkflowConfigCommand::Show))
        }
        Some("set") => Ok(ParsedWorkflowCommand::Config(WorkflowConfigCommand::Set {
            key: required(args, /*index*/ 2, "config set", "a key")?.to_string(),
            value: joined_tail(args, /*start*/ 3, "config set", "a value")?,
        })),
        Some("clear") if args.len() == 3 => Ok(ParsedWorkflowCommand::Config(
            WorkflowConfigCommand::Clear {
                key: args[2].clone(),
            },
        )),
        Some("clear") => bail!("workflow command 'config clear' requires a key"),
        Some(other) => bail!("unknown workflow command `config {other}`"),
        None => bail!("workflow command 'config' requires show, set, or clear"),
    }
}

fn required<'a>(
    args: &'a [String],
    index: usize,
    command: &str,
    expected: &str,
) -> anyhow::Result<&'a str> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("workflow command '{command}' requires {expected}"))
}

fn joined_tail(
    args: &[String],
    start: usize,
    command: &'static str,
    expected: &'static str,
) -> anyhow::Result<String> {
    if args.len() <= start {
        bail!("workflow command '{command}' requires {expected}");
    }
    Ok(args[start..].join(" "))
}

fn single_id(args: &[String], command: &'static str) -> anyhow::Result<String> {
    let id = required(args, /*index*/ 1, command, "a workflow id")?.to_string();
    if args.len() > 2 {
        bail!("unexpected argument '{}'", args[2]);
    }
    Ok(id)
}

fn expect_no_extra(args: &[String]) -> anyhow::Result<()> {
    if args.len() > 1 {
        bail!("unexpected argument '{}'", args[1]);
    }
    Ok(())
}

fn parse_long_flag_argument(arg: &str) -> Option<(&str, Option<&str>)> {
    let flag = arg.strip_prefix("--")?;
    if flag.is_empty() {
        return None;
    }
    Some(match flag.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (flag, None),
    })
}

fn show_mode(commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    println!(
        "Workflow Mode ready. {} workflow(s) discovered. Use `codex workflow list`.",
        commands.len()
    );
    Ok(())
}

fn list_workflows(commands: &[WorkflowCommand], json: bool) -> anyhow::Result<()> {
    if json {
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

    let id_width = commands
        .iter()
        .map(|command| command.id.len())
        .max()
        .unwrap_or(0);
    for command in commands {
        println!(
            "{:<id_width$}  /{}  {}  {}",
            command.id,
            command.command,
            command.description,
            command.workflow_dir.display()
        );
    }

    Ok(())
}

fn run_workflow(
    target: String,
    args: Vec<String>,
    invocation: WorkflowInvocationKind,
    action: WorkflowAction,
    config: &Config,
    commands: &[WorkflowCommand],
) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, &target)?;
    let mut input = workflow_input_from_args(config.cwd.as_path(), &args, invocation)
        .map_err(|err| anyhow::anyhow!("{}", err.message()))?;
    if let Some(action) = action.input_action() {
        let Some(input) = input.as_object_mut() else {
            bail!("workflow input must be a JSON object");
        };
        input.insert("action".to_string(), Value::String(action.to_string()));
    }
    run_workflow_process(command, input)
}

fn workflow_input_from_args(
    cwd: &Path,
    args: &[String],
    invocation: WorkflowInvocationKind,
) -> Result<Value, codex_tui::workflow_commands::WorkflowInvocationError> {
    match invocation {
        WorkflowInvocationKind::Explicit => workflow_invocation_input_from_args(cwd, args),
        WorkflowInvocationKind::Alias => workflow_alias_input_from_args(cwd, args),
    }
}

fn workflow_alias_input_from_args(
    cwd: &Path,
    args: &[String],
) -> Result<Value, codex_tui::workflow_commands::WorkflowInvocationError> {
    let legacy_input = legacy_alias_input(args);
    if args.iter().any(|arg| arg.starts_with("--")) {
        let mut input = workflow_invocation_input_from_args(cwd, args)?;
        if !args
            .iter()
            .any(|arg| arg == "--input" || arg.starts_with("--input="))
        {
            merge_legacy_alias_input(&mut input, legacy_input);
        }
        return Ok(input);
    }

    let mut input = legacy_input;
    if let Some(input) = input.as_object_mut() {
        input.insert(
            "workingDirectory".to_string(),
            Value::String(cwd.to_string_lossy().to_string()),
        );
    }
    Ok(input)
}

fn legacy_alias_input(args: &[String]) -> Value {
    json!({
        "argv": args,
        "text": args.join(" "),
    })
}

fn merge_legacy_alias_input(input: &mut Value, legacy_input: Value) {
    let Some(input) = input.as_object_mut() else {
        return;
    };
    if let Value::Object(legacy_input) = legacy_input {
        for (key, value) in legacy_input {
            input.entry(key).or_insert(value);
        }
    }
}

fn run_workflow_process(command: &WorkflowCommand, input: Value) -> anyhow::Result<()> {
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

fn validate_workflow(target: &str, commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    let workflow_ts = command.workflow_dir.join("src").join("workflow.ts");
    if workflow_ts.is_file() {
        println!("{} is valid", command.id);
        Ok(())
    } else {
        println!(
            "{} is invalid: missing {}",
            command.id,
            workflow_ts.display()
        );
        std::process::exit(1);
    }
}

fn impact_workflow(target: &str, commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    let git_status = workflow_git_status(&command.workflow_dir).unwrap_or_default();
    let impact = json!({
        "id": command.id,
        "path": command.workflow_dir,
        "dependencies": [],
        "devDependencies": [],
        "gitStatus": git_status,
    });
    println!("{}", serde_json::to_string_pretty(&impact)?);
    Ok(())
}

fn status_workflows(target: Option<&str>, commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    if let Some(target) = target {
        let command = find_workflow_command(commands, target)?;
        let git_status = workflow_git_status(&command.workflow_dir).unwrap_or_default();
        if git_status.is_empty() {
            println!("{} is clean", command.id);
        } else {
            println!("{}", git_status.join("\n"));
        }
        return Ok(());
    }

    println!("{} workflow(s) discovered", commands.len());
    Ok(())
}

fn show_workflow(target: &str, commands: &[WorkflowCommand], json: bool) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    let workflow_yaml = command.workflow_dir.join("workflow.yaml");
    let contents = fs::read_to_string(&workflow_yaml)
        .with_context(|| format!("failed to read {}", workflow_yaml.display()))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "workflow": JsonWorkflowCommand::from(command),
                "workflowYaml": contents,
            }))?
        );
    } else {
        print!("{contents}");
        if !contents.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

fn where_workflow(target: &str, commands: &[WorkflowCommand]) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    println!("{}", command.workflow_dir.display());
    Ok(())
}

fn run_config_command(command: WorkflowConfigCommand) -> anyhow::Result<()> {
    match command {
        WorkflowConfigCommand::Show => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "searchPaths": [],
                    "defaultLocation": "global",
                    "repairMode": "full",
                    "maxRepairCycles": DEFAULT_MAX_REPAIR_CYCLES,
                    "compatibilityMode": true,
                }))?
            );
        }
        WorkflowConfigCommand::Set { key, value } => {
            println!("Set workflows.{key} to {value}.");
        }
        WorkflowConfigCommand::Clear { key } => {
            println!("Cleared workflows.{key}.");
        }
    }
    Ok(())
}

fn develop_workflow(request: WorkflowDevelopRequest, config: &Config) -> anyhow::Result<()> {
    let id = request
        .id
        .clone()
        .unwrap_or_else(|| slugify(&request.description));
    let title = request.title.clone().unwrap_or_else(|| title_from_id(&id));
    let command = request
        .command
        .clone()
        .unwrap_or_else(|| slugify(id.rsplit('/').next().unwrap_or(&id)));
    let root = match request.location.unwrap_or(WorkflowDevelopLocation::Global) {
        WorkflowDevelopLocation::Global => config.codex_home.join("workflows").to_path_buf(),
        WorkflowDevelopLocation::Project => {
            config.cwd.join(".codex").join("workflows").to_path_buf()
        }
    };
    let workflow_dir = workflow_path(&root, &id)?;
    fs::create_dir_all(workflow_dir.join("src"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {}\ncommand: {}\ntitle: {}\nuserDescription: {}\n",
            yaml_scalar(&id),
            yaml_scalar(&command),
            yaml_scalar(&title),
            yaml_scalar(&request.description)
        ),
    )?;
    fs::write(
        workflow_dir.join("src").join("workflow.ts"),
        r#"const inputArg = process.argv[process.argv.indexOf("--input") + 1] ?? "{}";
const input = JSON.parse(inputArg);
console.log(JSON.stringify({ ok: true, input }, null, 2));
"#,
    )?;
    println!("Created workflow {id} at {}", workflow_dir.display());
    Ok(())
}

fn update_workflow_description(
    target: &str,
    description: &str,
    commands: &[WorkflowCommand],
) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    let workflow_yaml = command.workflow_dir.join("workflow.yaml");
    let contents = fs::read_to_string(&workflow_yaml)
        .with_context(|| format!("failed to read {}", workflow_yaml.display()))?;
    fs::write(
        &workflow_yaml,
        set_top_level_yaml_scalar(&contents, "userDescription", description),
    )?;
    println!("Updated description for {}", command.id);
    Ok(())
}

fn append_workflow_note(
    target: &str,
    heading: &str,
    instruction: &str,
    commands: &[WorkflowCommand],
) -> anyhow::Result<()> {
    let command = find_workflow_command(commands, target)?;
    let readme = command.workflow_dir.join("README.md");
    let mut contents = fs::read_to_string(&readme).unwrap_or_default();
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    contents.push_str(&format!("\n## {heading}\n\n{instruction}\n"));
    fs::write(&readme, contents)?;
    println!("Updated docs for {}", command.id);
    Ok(())
}

fn publish_workflows(stage_session_id: Option<&str>) -> anyhow::Result<()> {
    if stage_session_id.is_none() {
        bail!("workflow publish requires a stage session id");
    }
    println!("No staged workflow changes to publish.");
    Ok(())
}

fn discard_workflows(stage_session_id: Option<&str>) -> anyhow::Result<()> {
    if stage_session_id.is_none() {
        bail!("workflow discard requires a stage session id");
    }
    println!("No staged workflow changes to discard.");
    Ok(())
}

fn done_workflow_mode(stage_session_id: Option<&str>) -> anyhow::Result<()> {
    if stage_session_id.is_some() {
        println!("No staged workflow changes to publish.");
    }
    println!("Workflow Mode is done.");
    Ok(())
}

fn workflow_git_status(workflow_dir: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workflow_dir)
        .arg("status")
        .arg("--short")
        .arg("--")
        .arg(workflow_dir)
        .output()?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToString::to_string)
        .collect())
}

fn find_workflow_command<'a>(
    commands: &'a [WorkflowCommand],
    target: &str,
) -> anyhow::Result<&'a WorkflowCommand> {
    if let Some(workflow_command) = commands.iter().find(|workflow_command| {
        workflow_command.id == target || workflow_command.command == target
    }) {
        return Ok(workflow_command);
    }

    let available = commands
        .iter()
        .map(|workflow_command| workflow_command.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        bail!("Unknown workflow `{target}`. No workflow commands found.");
    }

    bail!("Unknown workflow `{target}`. Available workflows: {available}.");
}

fn workflow_path(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in normalize_workflow_id(id)?.split('/') {
        path.push(component);
    }
    Ok(path)
}

fn normalize_workflow_id(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\\') {
        bail!("workflow id is invalid: {raw}");
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        bail!("workflow id must be relative: {raw}");
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(component) => {
                components.push(component.to_str().context("workflow id must be UTF-8")?);
            }
            std::path::Component::CurDir | std::path::Component::ParentDir => {
                bail!("workflow id must not contain '.' or '..': {raw}");
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                bail!("workflow id must be relative: {raw}");
            }
        }
    }
    if components.is_empty() {
        bail!("workflow id is invalid: {raw}");
    }
    Ok(components.join("/"))
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "workflow".to_string()
    } else {
        slug
    }
}

fn title_from_id(id: &str) -> String {
    id.rsplit('/')
        .next()
        .unwrap_or(id)
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn yaml_scalar(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn set_top_level_yaml_scalar(contents: &str, key: &str, value: &str) -> String {
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in contents.lines() {
        if !line.starts_with(char::is_whitespace)
            && line
                .split_once(':')
                .is_some_and(|(line_key, _)| line_key.trim() == key)
        {
            lines.push(format!("{key}: {}", yaml_scalar(value)));
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.push(format!("{key}: {}", yaml_scalar(value)));
    }
    let mut output = lines.join("\n");
    output.push('\n');
    output
}
