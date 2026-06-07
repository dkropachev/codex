use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;

use thiserror::Error;

use crate::registry::WorkflowSummary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommand {
    Mode,
    Develop {
        request: WorkflowDevelopRequest,
    },
    Describe {
        id: String,
        description: String,
    },
    Docs {
        id: String,
        instruction: String,
    },
    Edit {
        id: String,
        instruction: String,
    },
    Fix {
        id: String,
    },
    Run {
        id: String,
        input: Option<WorkflowInputSource>,
        input_fields: BTreeMap<String, String>,
    },
    Validate {
        id: String,
    },
    Impact {
        id: String,
    },
    Status {
        id: Option<String>,
    },
    List,
    Show {
        id: String,
    },
    Where {
        id: String,
    },
    Config(WorkflowConfigCommand),
    Publish,
    Discard,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDevelopRequest {
    pub description: String,
    pub id: Option<String>,
    pub command: Option<String>,
    pub title: Option<String>,
    pub location: Option<WorkflowDevelopLocation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowDevelopLocation {
    Project,
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowInputSource {
    Inline(String),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowConfigCommand {
    Show,
    Set { key: String, value: String },
    Clear { key: String },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WorkflowCommandParseError {
    #[error("failed to parse workflow command")]
    InvalidCommandLine,
    #[error("unknown workflow command '{0}'")]
    UnknownCommand(String),
    #[error("workflow command '{0}' requires {1}")]
    MissingArgument(&'static str, &'static str),
    #[error("unexpected argument '{0}'")]
    UnexpectedArgument(String),
    #[error(
        "workflow develop metadata flags must be passed as separate arguments, for example `develop --location project --id <id> --command <command> <description>`"
    )]
    MisquotedDevelopFlags,
}

pub fn parse_workflow_command_line(
    command: &str,
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let args = shlex::split(command).ok_or(WorkflowCommandParseError::InvalidCommandLine)?;
    parse_workflow_command(&args)
}

pub fn parse_workflow_command_line_with_workflows(
    command: &str,
    workflows: &[WorkflowSummary],
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let args = shlex::split(command).ok_or(WorkflowCommandParseError::InvalidCommandLine)?;
    parse_workflow_command_with_workflows(&args, workflows)
}

pub fn parse_workflow_command(
    args: &[String],
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(WorkflowCommand::Mode);
    };
    match command {
        "develop" => parse_develop(args),
        "describe" => Ok(WorkflowCommand::Describe {
            id: required(args, /*index*/ 1, "describe", "a workflow id")?.to_string(),
            description: joined_tail(args, /*start*/ 2, "describe", "a description")?,
        }),
        "docs" => Ok(WorkflowCommand::Docs {
            id: required(args, /*index*/ 1, "docs", "a workflow id")?.to_string(),
            instruction: joined_tail(args, /*start*/ 2, "docs", "an instruction")?,
        }),
        "edit" => Ok(WorkflowCommand::Edit {
            id: required(args, /*index*/ 1, "edit", "a workflow id")?.to_string(),
            instruction: joined_tail(args, /*start*/ 2, "edit", "an instruction")?,
        }),
        "repair" => Ok(WorkflowCommand::Fix {
            id: single_id(args, "repair")?,
        }),
        "fix" => Ok(WorkflowCommand::Fix {
            id: single_id(args, "fix")?,
        }),
        "run" => parse_run(args),
        "validate" => Ok(WorkflowCommand::Validate {
            id: single_id(args, "validate")?,
        }),
        "impact" => Ok(WorkflowCommand::Impact {
            id: single_id(args, "impact")?,
        }),
        "status" => {
            if args.len() > 2 {
                return Err(WorkflowCommandParseError::UnexpectedArgument(
                    args[2].clone(),
                ));
            }
            Ok(WorkflowCommand::Status {
                id: args.get(1).cloned(),
            })
        }
        "list" => expect_no_extra(args, WorkflowCommand::List),
        "show" => Ok(WorkflowCommand::Show {
            id: single_id(args, "show")?,
        }),
        "where" => Ok(WorkflowCommand::Where {
            id: single_id(args, "where")?,
        }),
        "config" => parse_config(args),
        "publish" => expect_no_extra(args, WorkflowCommand::Publish),
        "discard" => expect_no_extra(args, WorkflowCommand::Discard),
        "done" => expect_no_extra(args, WorkflowCommand::Done),
        other => Err(WorkflowCommandParseError::UnknownCommand(other.to_string())),
    }
}

pub fn parse_workflow_command_with_workflows(
    args: &[String],
    workflows: &[WorkflowSummary],
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    match parse_workflow_command(args) {
        Ok(command) => Ok(command),
        Err(WorkflowCommandParseError::UnknownCommand(name)) => {
            parse_registered_workflow_command(args, &name, workflows)
        }
        Err(err) => Err(err),
    }
}

fn parse_develop(args: &[String]) -> Result<WorkflowCommand, WorkflowCommandParseError> {
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
            Some((flag, _)) => {
                if looks_like_quoted_develop_flags(&args[index]) {
                    return Err(WorkflowCommandParseError::MisquotedDevelopFlags);
                }
                return Err(WorkflowCommandParseError::UnexpectedArgument(format!(
                    "--{flag}"
                )));
            }
            None => {
                description_parts.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    if description_parts.is_empty() {
        return Err(WorkflowCommandParseError::MissingArgument(
            "develop",
            "a description",
        ));
    }

    Ok(WorkflowCommand::Develop {
        request: WorkflowDevelopRequest {
            description: description_parts.join(" "),
            id,
            command,
            title,
            location,
        },
    })
}

fn parse_develop_location(
    value: &str,
) -> Result<WorkflowDevelopLocation, WorkflowCommandParseError> {
    match value {
        "project" => Ok(WorkflowDevelopLocation::Project),
        "global" => Ok(WorkflowDevelopLocation::Global),
        _ => Err(WorkflowCommandParseError::UnexpectedArgument(format!(
            "--location {value}"
        ))),
    }
}

fn looks_like_quoted_develop_flags(value: &str) -> bool {
    value.starts_with("--")
        && value.chars().any(char::is_whitespace)
        && ["--id", "--command", "--title", "--location"]
            .iter()
            .any(|flag| value.contains(flag))
}

fn parse_registered_workflow_command(
    args: &[String],
    command_name: &str,
    workflows: &[WorkflowSummary],
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let Some(workflow) = crate::registry::find_workflow_by_command(workflows, command_name) else {
        return Err(WorkflowCommandParseError::UnknownCommand(
            command_name.to_string(),
        ));
    };
    let alias_args = args.get(1..).unwrap_or(&[]);
    let allowed_fields = command_input_fields_from_hints(&workflow.command_option_hints);
    let (input, input_fields) = parse_input_args(
        alias_args,
        "workflow alias",
        (!allowed_fields.is_empty()).then_some(&allowed_fields),
    )?;
    Ok(WorkflowCommand::Run {
        id: workflow.id.clone(),
        input,
        input_fields,
    })
}

fn parse_run(args: &[String]) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let id = required(args, /*index*/ 1, "run", "a workflow id")?.to_string();
    let (input, input_fields) = parse_input_args(&args[2..], "run", /*allowed_fields*/ None)?;
    Ok(WorkflowCommand::Run {
        id,
        input,
        input_fields,
    })
}

fn parse_input_args(
    args: &[String],
    command: &'static str,
    allowed_fields: Option<&BTreeSet<String>>,
) -> Result<(Option<WorkflowInputSource>, BTreeMap<String, String>), WorkflowCommandParseError> {
    let mut input = None;
    let mut input_fields = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        match parse_long_flag_argument(&args[index]) {
            Some(("input", Some(value))) => {
                input = Some(parse_workflow_input_source(value));
                index += 1;
            }
            Some(("input", None)) => {
                let value = required(args, index + 1, command, "JSON or @file")?;
                input = Some(parse_workflow_input_source(value));
                index += 2;
            }
            Some((field, inline_value)) => {
                let normalized_field = normalize_input_field_name(field);
                if allowed_fields
                    .is_some_and(|allowed_fields| !allowed_fields.contains(&normalized_field))
                {
                    return Err(WorkflowCommandParseError::UnexpectedArgument(format!(
                        "--{field}"
                    )));
                }
                let (value, consumed_args) = match inline_value {
                    Some(value) => (value.to_string(), 1),
                    None => match args.get(index + 1) {
                        Some(next) if !next.starts_with("--") => (next.to_string(), 2),
                        _ => ("true".to_string(), 1),
                    },
                };
                input_fields.insert(normalized_field, value);
                index += consumed_args;
            }
            None => {
                return Err(WorkflowCommandParseError::UnexpectedArgument(
                    args[index].clone(),
                ));
            }
        }
    }
    Ok((input, input_fields))
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

fn parse_workflow_input_source(value: &str) -> WorkflowInputSource {
    if let Some(path) = value.strip_prefix('@') {
        WorkflowInputSource::File(PathBuf::from(path))
    } else {
        WorkflowInputSource::Inline(value.to_string())
    }
}

fn normalize_input_field_name(flag: &str) -> String {
    crate::input_adapter::normalize_input_field_name(flag)
}

fn command_input_fields_from_hints(
    hints: &[crate::command_completion::WorkflowCommandOptionHint],
) -> BTreeSet<String> {
    hints
        .iter()
        .filter_map(|hint| {
            hint.display
                .split_whitespace()
                .next()
                .and_then(|flag| flag.strip_prefix("--"))
                .filter(|field| !field.is_empty())
                .map(normalize_input_field_name)
        })
        .collect()
}

fn parse_config(args: &[String]) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    match args.get(1).map(String::as_str) {
        Some("show") if args.len() == 2 => Ok(WorkflowCommand::Config(WorkflowConfigCommand::Show)),
        Some("set") => Ok(WorkflowCommand::Config(WorkflowConfigCommand::Set {
            key: required(args, /*index*/ 2, "config set", "a key")?.to_string(),
            value: joined_tail(args, /*start*/ 3, "config set", "a value")?,
        })),
        Some("clear") if args.len() == 3 => {
            Ok(WorkflowCommand::Config(WorkflowConfigCommand::Clear {
                key: args[2].clone(),
            }))
        }
        Some("clear") => Err(WorkflowCommandParseError::MissingArgument(
            "config clear",
            "a key",
        )),
        Some(other) => Err(WorkflowCommandParseError::UnknownCommand(format!(
            "config {other}"
        ))),
        None => Err(WorkflowCommandParseError::MissingArgument(
            "config",
            "show, set, or clear",
        )),
    }
}

fn required<'a>(
    args: &'a [String],
    index: usize,
    command: &'static str,
    expected: &'static str,
) -> Result<&'a str, WorkflowCommandParseError> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(WorkflowCommandParseError::MissingArgument(
            command, expected,
        ))
}

fn joined_tail(
    args: &[String],
    start: usize,
    command: &'static str,
    expected: &'static str,
) -> Result<String, WorkflowCommandParseError> {
    if args.len() <= start {
        return Err(WorkflowCommandParseError::MissingArgument(
            command, expected,
        ));
    }
    Ok(args[start..].join(" "))
}

fn single_id(args: &[String], command: &'static str) -> Result<String, WorkflowCommandParseError> {
    let id = required(args, /*index*/ 1, command, "a workflow id")?.to_string();
    if args.len() > 2 {
        return Err(WorkflowCommandParseError::UnexpectedArgument(
            args[2].clone(),
        ));
    }
    Ok(id)
}

fn expect_no_extra(
    args: &[String],
    command: WorkflowCommand,
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    if args.len() > 1 {
        return Err(WorkflowCommandParseError::UnexpectedArgument(
            args[1].clone(),
        ));
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_completion::WorkflowCommandOptionHint;
    use pretty_assertions::assert_eq;

    fn workflow_summary(command: Option<&str>) -> WorkflowSummary {
        workflow_summary_with_hints(command, Vec::new())
    }

    fn workflow_summary_with_hints(
        command: Option<&str>,
        command_option_hints: Vec<WorkflowCommandOptionHint>,
    ) -> WorkflowSummary {
        WorkflowSummary {
            id: "reports/jira-summary".to_string(),
            command: command.map(ToString::to_string),
            title: Some("Jira Summary".to_string()),
            user_description: Some("Prepare a concise Jira summary".to_string()),
            search_terms: vec!["jira".to_string()],
            command_option_hints,
            root_label: "global".to_string(),
            root_kind: crate::registry::WorkflowRootKind::Global,
            root_path: PathBuf::from("/tmp/workflows"),
            path: PathBuf::from("/tmp/workflows/reports/jira-summary"),
            workflow_yaml_path: PathBuf::from("/tmp/workflows/reports/jira-summary/workflow.yaml"),
            mention_target: "workflow:///tmp/workflows/reports/jira-summary#reports/jira-summary"
                .to_string(),
            validation: crate::registry::WorkflowValidation {
                status: crate::registry::WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        }
    }

    #[test]
    fn parses_shared_workflow_commands() {
        assert_eq!(parse_workflow_command(&[]).unwrap(), WorkflowCommand::Mode);
        assert_eq!(
            parse_workflow_command_line("run reports/jira --input '{\"project\":\"COD\"}'")
                .unwrap(),
            WorkflowCommand::Run {
                id: "reports/jira".to_string(),
                input: Some(WorkflowInputSource::Inline(
                    "{\"project\":\"COD\"}".to_string()
                )),
                input_fields: BTreeMap::new(),
            }
        );
        assert_eq!(
            parse_workflow_command_line("publish").unwrap(),
            WorkflowCommand::Publish
        );
        assert_eq!(
            parse_workflow_command_line(
                "develop --id pr-triage --command review-pr --title 'PR Triage' Analyze a PR"
            )
            .unwrap(),
            WorkflowCommand::Develop {
                request: WorkflowDevelopRequest {
                    description: "Analyze a PR".to_string(),
                    id: Some("pr-triage".to_string()),
                    command: Some("review-pr".to_string()),
                    title: Some("PR Triage".to_string()),
                    location: None,
                },
            }
        );
        assert_eq!(
            parse_workflow_command_line("develop --location project Jira Summary").unwrap(),
            WorkflowCommand::Develop {
                request: WorkflowDevelopRequest {
                    description: "Jira Summary".to_string(),
                    id: None,
                    command: None,
                    title: None,
                    location: Some(WorkflowDevelopLocation::Project),
                },
            }
        );
        assert_eq!(
            parse_workflow_command_line("develop Jira Summary").unwrap(),
            WorkflowCommand::Develop {
                request: WorkflowDevelopRequest {
                    description: "Jira Summary".to_string(),
                    id: None,
                    command: None,
                    title: None,
                    location: None,
                },
            }
        );
        assert_eq!(
            parse_workflow_command_line("develop '--id jira --command jira Review Jira'"),
            Err(WorkflowCommandParseError::MisquotedDevelopFlags)
        );
        assert_eq!(
            parse_workflow_command_line("discard").unwrap(),
            WorkflowCommand::Discard
        );
        assert_eq!(
            parse_workflow_command_line("repair reports/jira").unwrap(),
            WorkflowCommand::Fix {
                id: "reports/jira".to_string(),
            }
        );
        assert_eq!(
            parse_workflow_command_line("config set repair_mode threshold:2").unwrap(),
            WorkflowCommand::Config(WorkflowConfigCommand::Set {
                key: "repair_mode".to_string(),
                value: "threshold:2".to_string(),
            })
        );
    }

    #[test]
    fn parses_registered_workflow_alias_into_run_command() {
        let workflows = vec![workflow_summary(Some("jira-summary"))];
        let command = parse_workflow_command_with_workflows(
            &[
                "jira-summary".to_string(),
                "--project".to_string(),
                "COD".to_string(),
            ],
            &workflows,
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "reports/jira-summary".to_string(),
                input: None,
                input_fields: BTreeMap::from([("project".to_string(), "COD".to_string())]),
            }
        );
    }

    #[test]
    fn registered_workflow_alias_rejects_positional_args() {
        let workflows = vec![workflow_summary(Some("jira-summary"))];
        let err = parse_workflow_command_with_workflows(
            &["jira-summary".to_string(), "current sprint".to_string()],
            &workflows,
        )
        .expect_err("positional workflow args should fail");

        assert_eq!(
            err,
            WorkflowCommandParseError::UnexpectedArgument("current sprint".to_string())
        );
    }

    #[test]
    fn parses_registered_workflow_alias_flags_against_option_hints() {
        let workflows = vec![workflow_summary_with_hints(
            Some("patch-impact"),
            vec![
                WorkflowCommandOptionHint {
                    display: "--base-ref <string>".to_string(),
                    description: None,
                },
                WorkflowCommandOptionHint {
                    display: "--include-untracked".to_string(),
                    description: None,
                },
                WorkflowCommandOptionHint {
                    display: "--max-files <integer>".to_string(),
                    description: None,
                },
            ],
        )];
        let command = parse_workflow_command_with_workflows(
            &[
                "patch-impact".to_string(),
                "--base-ref".to_string(),
                "HEAD".to_string(),
                "--include-untracked".to_string(),
                "--max-files".to_string(),
                "20".to_string(),
            ],
            &workflows,
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "reports/jira-summary".to_string(),
                input: None,
                input_fields: BTreeMap::from([
                    ("baseRef".to_string(), "HEAD".to_string()),
                    ("includeUntracked".to_string(), "true".to_string()),
                    ("maxFiles".to_string(), "20".to_string()),
                ]),
            }
        );
    }

    #[test]
    fn registered_workflow_alias_rejects_unknown_flags_when_hints_are_known() {
        let workflows = vec![workflow_summary_with_hints(
            Some("patch-impact"),
            vec![WorkflowCommandOptionHint {
                display: "--base-ref <string>".to_string(),
                description: None,
            }],
        )];
        let err = parse_workflow_command_with_workflows(
            &[
                "patch-impact".to_string(),
                "--unknown".to_string(),
                "value".to_string(),
            ],
            &workflows,
        )
        .expect_err("unknown hinted alias flag should fail");

        assert_eq!(
            err,
            WorkflowCommandParseError::UnexpectedArgument("--unknown".to_string())
        );
    }

    #[test]
    fn registered_workflow_alias_rejects_incomplete_hint_prefixes() {
        let workflows = vec![workflow_summary_with_hints(
            Some("code-review"),
            vec![WorkflowCommandOptionHint {
                display: "--assignee <string>".to_string(),
                description: None,
            }],
        )];
        let err = parse_workflow_command_with_workflows(
            &["code-review".to_string(), "--a".to_string()],
            &workflows,
        )
        .expect_err("incomplete hinted alias flag should fail");

        assert_eq!(
            err,
            WorkflowCommandParseError::UnexpectedArgument("--a".to_string())
        );
    }

    #[test]
    fn parses_registered_workflow_alias_input_json() {
        let workflows = vec![workflow_summary(Some("patch-impact"))];
        let command = parse_workflow_command_with_workflows(
            &[
                "patch-impact".to_string(),
                "--input".to_string(),
                r#"{"includeUntracked":true}"#.to_string(),
            ],
            &workflows,
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "reports/jira-summary".to_string(),
                input: Some(WorkflowInputSource::Inline(
                    r#"{"includeUntracked":true}"#.to_string()
                )),
                input_fields: BTreeMap::new(),
            }
        );
    }

    #[test]
    fn parses_run_command_input_flags_into_json_fields() {
        let command = parse_workflow_command_line(
            "run review/fix --workingDirectory /tmp/repo --scope repo --reviewMode initial --include-untracked",
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "review/fix".to_string(),
                input: None,
                input_fields: BTreeMap::from([
                    ("includeUntracked".to_string(), "true".to_string()),
                    ("reviewMode".to_string(), "initial".to_string()),
                    ("scope".to_string(), "repo".to_string()),
                    ("workingDirectory".to_string(), "/tmp/repo".to_string()),
                ]),
            }
        );
    }

    #[test]
    fn parses_run_command_input_flags_with_inline_json_input() {
        let command = parse_workflow_command_line(
            "run review/fix --input '{\"scope\":\"repo\"}' --working-directory /tmp/repo",
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "review/fix".to_string(),
                input: Some(WorkflowInputSource::Inline(
                    "{\"scope\":\"repo\"}".to_string()
                )),
                input_fields: BTreeMap::from([(
                    "workingDirectory".to_string(),
                    "/tmp/repo".to_string(),
                )]),
            }
        );
    }

    #[test]
    fn parses_run_command_input_flags_with_equals_syntax() {
        let command = parse_workflow_command_line(
            "run review/fix --workingDirectory=/tmp/repo --reviewMode=initial",
        )
        .unwrap();

        assert_eq!(
            command,
            WorkflowCommand::Run {
                id: "review/fix".to_string(),
                input: None,
                input_fields: BTreeMap::from([
                    ("reviewMode".to_string(), "initial".to_string()),
                    ("workingDirectory".to_string(), "/tmp/repo".to_string()),
                ]),
            }
        );
    }
}
