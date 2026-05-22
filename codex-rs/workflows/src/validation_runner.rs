use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowValidationCommandResult {
    pub(crate) command: String,
    pub(crate) succeeded: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkflowValidationReport {
    pub(crate) status: crate::registry::WorkflowValidationStatus,
    pub(crate) messages: Vec<String>,
    pub(crate) command_results: Vec<WorkflowValidationCommandResult>,
}

pub(crate) fn validate_workflow<F>(
    workflow: &crate::registry::WorkflowSummary,
    mut command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let mut messages = workflow.validation.messages.clone();
    let mut command_results = Vec::new();

    if let Ok(spec) = crate::spec::read_workflow_spec(&workflow.workflow_yaml_path) {
        for command in validation_commands(&spec) {
            let result = command_runner(&command, &workflow.path)?;
            let command_failed = !result.succeeded;
            if command_failed {
                messages.push(format!(
                    "validation command `{command}` failed with {}",
                    exit_status_label(result.exit_code)
                ));
            }
            command_results.push(result);
            if command_failed {
                break;
            }
        }
    }

    let status = if messages.is_empty() {
        crate::registry::WorkflowValidationStatus::Valid
    } else {
        crate::registry::WorkflowValidationStatus::Invalid
    };
    Ok(WorkflowValidationReport {
        status,
        messages,
        command_results,
    })
}

pub(crate) fn validation_report_message(report: &WorkflowValidationReport) -> String {
    if report.messages.is_empty() {
        "valid".to_string()
    } else {
        report.messages.join("\n")
    }
}

pub(crate) fn run_validation_command(
    command: &str,
    cwd: &Path,
) -> Result<WorkflowValidationCommandResult> {
    let output = validation_shell_command(command)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run validation command `{command}`"))?;
    Ok(WorkflowValidationCommandResult {
        command: command.to_string(),
        succeeded: output.status.success(),
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn validation_commands(spec: &crate::spec::WorkflowSpec) -> Vec<String> {
    let commands = spec
        .validation
        .get("commands")
        .and_then(JsonValue::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if commands.is_empty() {
        vec!["npm test".to_string()]
    } else {
        commands
    }
}

fn validation_shell_command(command: &str) -> Command {
    if cfg!(windows) {
        let mut process = Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = Command::new("sh");
        process.args(["-lc", command]);
        process
    }
}

fn exit_status_label(exit_code: Option<i32>) -> String {
    exit_code
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "a non-zero status".to_string())
}
