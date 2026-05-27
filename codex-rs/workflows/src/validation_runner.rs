use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::spec::WorkflowRuntimeKind;
use crate::spec::normalize_runtime_entrypoint;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_finding::finding_messages;

pub(crate) const RUNE_BUILTIN_TEST_COMMAND: &str = "codex workflow test-rune";

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
    pub(crate) findings: Vec<WorkflowValidationFinding>,
    pub(crate) command_results: Vec<WorkflowValidationCommandResult>,
}

impl WorkflowValidationReport {
    pub(crate) fn from_findings(
        findings: Vec<WorkflowValidationFinding>,
        command_results: Vec<WorkflowValidationCommandResult>,
    ) -> Self {
        let status = if findings.is_empty() {
            crate::registry::WorkflowValidationStatus::Valid
        } else {
            crate::registry::WorkflowValidationStatus::Invalid
        };
        Self {
            status,
            findings,
            command_results,
        }
    }

    pub(crate) fn push_finding(&mut self, finding: WorkflowValidationFinding) {
        self.findings.push(finding);
        self.status = crate::registry::WorkflowValidationStatus::Invalid;
    }
}

pub(crate) fn validate_workflow<F>(
    workflow: &crate::registry::WorkflowSummary,
    mut command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let mut findings = workflow.validation.findings.clone();
    let mut command_results = Vec::new();

    if let Ok(spec) = crate::spec::read_workflow_spec(&workflow.workflow_yaml_path) {
        let mut run_validation_commands = true;
        if workflow.runtime.kind == WorkflowRuntimeKind::Rune {
            let workflow_entrypoint =
                match normalize_runtime_entrypoint(&workflow.runtime.entrypoint) {
                    Ok(entrypoint) => entrypoint,
                    Err(_) => {
                        findings.push(WorkflowValidationFinding::WorkflowPathEscapesRoot {
                            workflow_path: workflow.path.join(&workflow.runtime.entrypoint),
                            root_path: workflow.path.clone(),
                        });
                        return Ok(WorkflowValidationReport::from_findings(
                            findings,
                            command_results,
                        ));
                    }
                };
            let workflow_path = workflow.path.join(&workflow_entrypoint);
            if workflow_path.is_file() {
                if let Err(err) = crate::rune_runtime::validate_workflow_source(&workflow_path) {
                    findings.push(WorkflowValidationFinding::WorkflowRuntimeCompileFailed {
                        path: workflow_entrypoint,
                        error: err.to_string(),
                    });
                    run_validation_commands = false;
                }
            } else {
                run_validation_commands = false;
            }
        }

        if run_validation_commands {
            for command in validation_commands(&spec) {
                let result = match command {
                    ValidationCommand::RuneTests => run_rune_test_command(workflow),
                    ValidationCommand::Shell(command) => command_runner(&command, &workflow.path),
                }?;
                let command_failed = !result.succeeded;
                if command_failed {
                    findings.push(WorkflowValidationFinding::ValidationCommandFailed {
                        command: result.command.clone(),
                        exit_code: result.exit_code,
                        stdout: result.stdout.clone(),
                        stderr: result.stderr.clone(),
                    });
                }
                command_results.push(result);
                if command_failed {
                    break;
                }
            }
        }
    }

    Ok(WorkflowValidationReport::from_findings(
        findings,
        command_results,
    ))
}

pub(crate) fn validation_report_message(report: &WorkflowValidationReport) -> String {
    let messages = finding_messages(&report.findings);
    if messages.is_empty() {
        "valid".to_string()
    } else {
        messages.join("\n")
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

enum ValidationCommand {
    RuneTests,
    Shell(String),
}

fn validation_commands(spec: &crate::spec::WorkflowSpec) -> Vec<ValidationCommand> {
    let commands = spec
        .validation
        .get("commands")
        .and_then(JsonValue::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(JsonValue::as_str)
                .map(|command| validation_command_for_spec(spec, command))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if commands.is_empty() {
        match spec.resolved_runtime().kind {
            WorkflowRuntimeKind::Rune => vec![ValidationCommand::RuneTests],
            WorkflowRuntimeKind::Typescript => {
                vec![ValidationCommand::Shell("npm test".to_string())]
            }
        }
    } else {
        commands
    }
}

fn validation_command_for_spec(
    spec: &crate::spec::WorkflowSpec,
    command: &str,
) -> ValidationCommand {
    if spec.resolved_runtime().kind == WorkflowRuntimeKind::Rune
        && matches!(command.trim(), RUNE_BUILTIN_TEST_COMMAND | "true")
    {
        return ValidationCommand::RuneTests;
    }
    ValidationCommand::Shell(command.to_string())
}

fn run_rune_test_command(
    workflow: &crate::registry::WorkflowSummary,
) -> Result<WorkflowValidationCommandResult> {
    match crate::rune_runtime::validate_workflow_tests(&workflow.path, &workflow.runtime.entrypoint)
    {
        Ok(report) => Ok(WorkflowValidationCommandResult {
            command: RUNE_BUILTIN_TEST_COMMAND.to_string(),
            succeeded: true,
            exit_code: Some(0),
            stdout: format!(
                "compiled workflow and ran {} Rune test(s)\n",
                report.tests_run
            ),
            stderr: String::new(),
        }),
        Err(err) => Ok(WorkflowValidationCommandResult {
            command: RUNE_BUILTIN_TEST_COMMAND.to_string(),
            succeeded: false,
            exit_code: None,
            stdout: String::new(),
            stderr: err.to_string(),
        }),
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
