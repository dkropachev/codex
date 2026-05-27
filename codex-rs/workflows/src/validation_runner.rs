use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use anyhow::Result;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_finding::finding_messages;
use crate::validation_package::is_bun_runtime_command;

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
        for command in validation_commands(&spec) {
            let result = command_runner(&command, &workflow.path)?;
            let command_failed = !result.succeeded;
            if command_failed {
                findings.push(WorkflowValidationFinding::ValidationCommandFailed {
                    command: command.clone(),
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
    let mut shell_command = validation_shell_command(command);
    let local_bun = crate::workflow_runtime::workflow_bun_path(cwd);
    if !local_bun.is_file() {
        crate::managed_bun::prepend_managed_bun_to_path(
            &mut shell_command,
            /*cache_root*/ None,
        )?;
    }
    prepend_workflow_bin_to_path(&mut shell_command, cwd)?;
    let output = shell_command
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
    match spec
        .validation
        .get("commands")
        .and_then(JsonValue::as_array)
    {
        Some(commands) => commands
            .iter()
            .filter_map(JsonValue::as_str)
            .filter(|command| is_bun_runtime_command(command))
            .map(ToString::to_string)
            .collect(),
        None => vec!["bun test src/tests".to_string()],
    }
}

fn prepend_workflow_bin_to_path(command: &mut Command, cwd: &Path) -> Result<()> {
    let bin_dir = cwd.join("node_modules/.bin");
    if !bin_dir.is_dir() {
        return Ok(());
    }
    let mut paths = vec![bin_dir];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    let paths = std::env::join_paths(paths).context("failed to build workflow validation PATH")?;
    command.env("PATH", paths);
    Ok(())
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
