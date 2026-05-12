use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowCommand {
    Mode,
    Develop {
        description: String,
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
    Done,
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
}

pub fn parse_workflow_command_line(
    command: &str,
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let args = shlex::split(command).ok_or(WorkflowCommandParseError::InvalidCommandLine)?;
    parse_workflow_command(&args)
}

pub fn parse_workflow_command(
    args: &[String],
) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(WorkflowCommand::Mode);
    };
    match command {
        "develop" => Ok(WorkflowCommand::Develop {
            description: joined_tail(args, 1, "develop", "a description")?,
        }),
        "describe" => Ok(WorkflowCommand::Describe {
            id: required(args, 1, "describe", "a workflow id")?.to_string(),
            description: joined_tail(args, 2, "describe", "a description")?,
        }),
        "docs" => Ok(WorkflowCommand::Docs {
            id: required(args, 1, "docs", "a workflow id")?.to_string(),
            instruction: joined_tail(args, 2, "docs", "an instruction")?,
        }),
        "edit" => Ok(WorkflowCommand::Edit {
            id: required(args, 1, "edit", "a workflow id")?.to_string(),
            instruction: joined_tail(args, 2, "edit", "an instruction")?,
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
        "done" => expect_no_extra(args, WorkflowCommand::Done),
        other => Err(WorkflowCommandParseError::UnknownCommand(other.to_string())),
    }
}

fn parse_run(args: &[String]) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    let id = required(args, 1, "run", "a workflow id")?.to_string();
    let mut input = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--input" => {
                let value = required(args, index + 1, "run --input", "JSON or @file")?;
                input = Some(if let Some(path) = value.strip_prefix('@') {
                    WorkflowInputSource::File(PathBuf::from(path))
                } else {
                    WorkflowInputSource::Inline(value.to_string())
                });
                index += 2;
            }
            value => {
                return Err(WorkflowCommandParseError::UnexpectedArgument(
                    value.to_string(),
                ));
            }
        }
    }
    Ok(WorkflowCommand::Run { id, input })
}

fn parse_config(args: &[String]) -> Result<WorkflowCommand, WorkflowCommandParseError> {
    match args.get(1).map(String::as_str) {
        Some("show") if args.len() == 2 => Ok(WorkflowCommand::Config(WorkflowConfigCommand::Show)),
        Some("set") => Ok(WorkflowCommand::Config(WorkflowConfigCommand::Set {
            key: required(args, 2, "config set", "a key")?.to_string(),
            value: joined_tail(args, 3, "config set", "a value")?,
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
    let id = required(args, 1, command, "a workflow id")?.to_string();
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
    use pretty_assertions::assert_eq;

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
}
