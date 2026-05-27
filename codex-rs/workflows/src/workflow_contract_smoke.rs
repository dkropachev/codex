use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use serde_json::Value as JsonValue;

use crate::WorkflowCommandInput;
use crate::api_contract::WorkflowSourceContract;
use crate::registry::WorkflowSummary;
use crate::spec::WorkflowRuntimeKind;
use crate::spec::WorkflowSpec;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_runner::WorkflowValidationCommandResult;
use crate::validation_runner::WorkflowValidationReport;
use crate::workflow_contract_validation::validate_json_against_schema;
use crate::workflow_contract_validation::validate_json_schema_document;

enum ContractSmokeCommand {
    Shell(String),
    RuneInput(JsonValue),
}

pub(crate) struct ContractSmokeCase {
    name: String,
    command: ContractSmokeCommand,
    input: Option<JsonValue>,
    expect_error: Option<String>,
    expect_output: Option<JsonValue>,
}

pub(crate) fn validate_source_contract_schemas(
    source_contract: &WorkflowSourceContract,
) -> Result<()> {
    validate_json_schema_document(&source_contract.input_schema, "api.inputSchema")?;
    validate_json_schema_document(&source_contract.output_schema, "api.outputSchema")?;
    for (format_name, schema) in &source_contract.format_schemas {
        validate_json_schema_document(schema, &format!("api.formatSchemas.{format_name}"))?;
    }
    Ok(())
}

pub(crate) fn run_contract_smoke_case<F>(
    workflow: &WorkflowSummary,
    working_directory: &Path,
    source_contract: &WorkflowSourceContract,
    case: &ContractSmokeCase,
    command_runner: &mut F,
    report: &mut WorkflowValidationReport,
) -> Result<()>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    if let Some(input) = &case.input
        && case.expect_error.is_none()
        && let Err(err) = validate_json_against_schema(&source_contract.input_schema, input)
    {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: contract_smoke_case_command_name(case),
            error: format!("input did not match the workflow contract: {err}"),
            stdout: String::new(),
            stderr: String::new(),
        });
        return Ok(());
    }

    match &case.command {
        ContractSmokeCommand::Shell(command) => run_contract_smoke_command(
            workflow,
            source_contract,
            case,
            command,
            command_runner,
            report,
        ),
        ContractSmokeCommand::RuneInput(input) => run_rune_contract_smoke(
            workflow,
            working_directory,
            source_contract,
            case,
            input,
            report,
        ),
    }
}

fn run_contract_smoke_command<F>(
    workflow: &WorkflowSummary,
    source_contract: &WorkflowSourceContract,
    case: &ContractSmokeCase,
    command: &str,
    command_runner: &mut F,
    report: &mut WorkflowValidationReport,
) -> Result<()>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let result = command_runner(command, &workflow.path)?;
    let succeeded = result.succeeded;
    let exit_code = result.exit_code;
    let stdout = result.stdout.clone();
    let stderr = result.stderr.clone();
    report.command_results.push(result);

    if let Some(expected) = &case.expect_error {
        validate_expected_smoke_error(
            command,
            expected,
            succeeded,
            &stdout,
            &stderr,
            exit_code.map(|code| format!("exit code {code}")).as_deref(),
            report,
        );
        return Ok(());
    }

    if !succeeded {
        let error = exit_code
            .map(|exit_code| format!("command exited with status {exit_code}"))
            .unwrap_or_else(|| "command exited unsuccessfully".to_string());
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error,
            stdout,
            stderr,
        });
        return Ok(());
    }

    let output = match serde_json::from_str::<JsonValue>(stdout.trim()) {
        Ok(output) => output,
        Err(err) => {
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: command.to_string(),
                error: format!("stdout was not valid JSON: {err}"),
                stdout,
                stderr,
            });
            return Ok(());
        }
    };

    if let Some(expected) = &case.expect_output
        && &output != expected
    {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("stdout JSON did not match expected output: expected {expected:?}"),
            stdout,
            stderr,
        });
        return Ok(());
    }

    if let Err(err) = validate_json_against_schema(&source_contract.output_schema, &output) {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("output did not match the workflow contract: {err}"),
            stdout,
            stderr,
        });
    }

    Ok(())
}

fn run_rune_contract_smoke(
    workflow: &WorkflowSummary,
    working_directory: &Path,
    source_contract: &WorkflowSourceContract,
    case: &ContractSmokeCase,
    input: &JsonValue,
    report: &mut WorkflowValidationReport,
) -> Result<()> {
    let command = rune_contract_smoke_command_name(&case.name);
    let input_json = serde_json::to_string(input)
        .with_context(|| "failed to serialize validation.contractSmoke.input")?;
    let workflow_entrypoint =
        crate::spec::normalize_runtime_entrypoint(&workflow.runtime.entrypoint)?;
    let output = crate::rune_runtime::run_workflow_for_validation(
        working_directory,
        &workflow.path,
        &workflow.path.join(workflow_entrypoint),
        &input_json,
    )?;
    let succeeded = output.success;
    let stdout = output.stdout;
    let stderr = output.stderr;
    let exit_status = output.exit_status;
    report
        .command_results
        .push(WorkflowValidationCommandResult {
            command: command.to_string(),
            succeeded,
            exit_code: None,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
        });

    if let Some(expected) = &case.expect_error {
        validate_expected_smoke_error(
            &command,
            expected,
            succeeded,
            &stdout,
            &stderr,
            Some(exit_status.as_str()),
            report,
        );
        return Ok(());
    }

    if !succeeded {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("runtime exited with {exit_status}"),
            stdout,
            stderr,
        });
        return Ok(());
    }

    let output = match serde_json::from_str::<JsonValue>(stdout.trim()) {
        Ok(output) => output,
        Err(err) => {
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: command.to_string(),
                error: format!("stdout was not valid JSON: {err}"),
                stdout,
                stderr,
            });
            return Ok(());
        }
    };

    if let Some(expected) = &case.expect_output
        && &output != expected
    {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("stdout JSON did not match expected output: expected {expected:?}"),
            stdout,
            stderr,
        });
        return Ok(());
    }

    if let Err(err) = validate_json_against_schema(&source_contract.output_schema, &output) {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("output did not match the workflow contract: {err}"),
            stdout,
            stderr,
        });
        return Ok(());
    }

    validate_rune_format_smoke(
        workflow,
        source_contract,
        &output,
        &command,
        stdout,
        stderr,
        report,
    )?;

    Ok(())
}

fn validate_expected_smoke_error(
    command: &str,
    expected: &str,
    succeeded: bool,
    stdout: &str,
    stderr: &str,
    exit_status: Option<&str>,
    report: &mut WorkflowValidationReport,
) {
    if succeeded {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: "expected failure but command succeeded".to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        });
        return;
    }

    if !expected.is_empty()
        && !stdout.contains(expected)
        && !stderr.contains(expected)
        && !exit_status.is_some_and(|status| status.contains(expected))
    {
        report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
            command: command.to_string(),
            error: format!("expected failure containing `{expected}`"),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        });
    }
}

fn validate_rune_format_smoke(
    workflow: &WorkflowSummary,
    source_contract: &WorkflowSourceContract,
    output: &JsonValue,
    command: &str,
    stdout: String,
    stderr: String,
    report: &mut WorkflowValidationReport,
) -> Result<()> {
    if source_contract.format_schemas.is_empty() {
        return Ok(());
    }

    let workflow_entrypoint =
        crate::spec::normalize_runtime_entrypoint(&workflow.runtime.entrypoint)?;
    let workflow_path = workflow.path.join(workflow_entrypoint);
    for (format_name, schema) in &source_contract.format_schemas {
        let formatted = match crate::rune_runtime::format_workflow_result_for_validation(
            &workflow_path,
            output.clone(),
            format_name,
        ) {
            Ok(Some(formatted)) => formatted,
            Ok(None) => {
                report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                    command: command.to_string(),
                    error: format!(
                        "workflow declares format `{format_name}` but does not define a formatter"
                    ),
                    stdout,
                    stderr,
                });
                return Ok(());
            }
            Err(err) => {
                report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                    command: command.to_string(),
                    error: format!("workflow format `{format_name}` failed: {err}"),
                    stdout,
                    stderr,
                });
                return Ok(());
            }
        };

        if let Err(err) = validate_json_against_schema(schema, &formatted) {
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: command.to_string(),
                error: format!("workflow format `{format_name}` did not match the contract: {err}"),
                stdout,
                stderr,
            });
            return Ok(());
        }
    }

    Ok(())
}

pub(crate) fn run_rune_autocomplete_smoke(
    workflow: &WorkflowSummary,
    working_directory: &Path,
    report: &mut WorkflowValidationReport,
) -> Result<()> {
    let command = "Rune validation.autocomplete";
    let workflow_entrypoint =
        crate::spec::normalize_runtime_entrypoint(&workflow.runtime.entrypoint)?;
    let result = crate::rune_runtime::complete_workflow_for_validation(
        &workflow.path,
        working_directory,
        &workflow.path.join(workflow_entrypoint),
        &WorkflowCommandInput {
            argv: Vec::new(),
            text: String::new(),
        },
    );

    match result {
        Ok(result) if result.complete_defined => {
            let stdout = serde_json::to_string_pretty(&result.suggestions)?;
            report
                .command_results
                .push(WorkflowValidationCommandResult {
                    command: command.to_string(),
                    succeeded: true,
                    exit_code: Some(0),
                    stdout,
                    stderr: String::new(),
                });
        }
        Ok(result) => {
            let stdout = serde_json::to_string_pretty(&result.suggestions)?;
            report
                .command_results
                .push(WorkflowValidationCommandResult {
                    command: command.to_string(),
                    succeeded: false,
                    exit_code: None,
                    stdout: stdout.clone(),
                    stderr: String::new(),
                });
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: command.to_string(),
                error: "validation.coverage.autocomplete is true but complete(ctx, input) is not defined".to_string(),
                stdout,
                stderr: String::new(),
            });
        }
        Err(err) => {
            report
                .command_results
                .push(WorkflowValidationCommandResult {
                    command: command.to_string(),
                    succeeded: false,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: err.to_string(),
                });
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: command.to_string(),
                error: format!("autocomplete validation failed: {err}"),
                stdout: String::new(),
                stderr: err.to_string(),
            });
        }
    }
    Ok(())
}

pub(crate) fn contract_smoke_cases(spec: &WorkflowSpec) -> Result<Vec<ContractSmokeCase>> {
    let runtime = spec.resolved_runtime().kind;
    let Some(smoke) = spec.validation.get("contractSmoke") else {
        if runtime == WorkflowRuntimeKind::Typescript {
            return Ok(Vec::new());
        }
        anyhow::bail!("Rune workflows must define validation.contractSmoke");
    };

    match smoke {
        JsonValue::Null | JsonValue::Bool(false) => disabled_contract_smoke_cases(runtime),
        JsonValue::Bool(true) => default_contract_smoke_case(
            runtime,
            "validation.contractSmoke".to_string(),
            JsonValue::Object(Default::default()),
            None,
            None,
        )
        .map(|case| vec![case]),
        JsonValue::String(command) => non_empty_contract_smoke_command(command).map(|command| {
            vec![ContractSmokeCase {
                name: "validation.contractSmoke".to_string(),
                command: ContractSmokeCommand::Shell(command),
                input: None,
                expect_error: None,
                expect_output: None,
            }]
        }),
        JsonValue::Object(object) => {
            if object.get("enabled") == Some(&JsonValue::Bool(false)) {
                return disabled_contract_smoke_cases(runtime);
            }
            if let Some(cases) = object.get("cases") {
                return contract_smoke_cases_from_array(runtime, cases);
            }
            contract_smoke_case_from_object(runtime, "validation.contractSmoke".to_string(), object)
                .map(|case| vec![case])
        }
        _ => Err(anyhow::anyhow!(
            "validation.contractSmoke must be false, true, a command string, or an object"
        )),
    }
}

fn disabled_contract_smoke_cases(runtime: WorkflowRuntimeKind) -> Result<Vec<ContractSmokeCase>> {
    if runtime == WorkflowRuntimeKind::Rune {
        anyhow::bail!("Rune workflows must enable validation.contractSmoke");
    }
    Ok(Vec::new())
}

fn contract_smoke_cases_from_array(
    runtime: WorkflowRuntimeKind,
    cases: &JsonValue,
) -> Result<Vec<ContractSmokeCase>> {
    let Some(cases) = cases.as_array() else {
        anyhow::bail!("validation.contractSmoke.cases must be an array");
    };
    if cases.is_empty() {
        anyhow::bail!("validation.contractSmoke.cases must not be empty");
    }

    cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            let Some(object) = case.as_object() else {
                anyhow::bail!("validation.contractSmoke.cases[{index}] must be an object");
            };
            let name = object
                .get("name")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("case {}", index + 1));
            contract_smoke_case_from_object(runtime, name, object)
        })
        .collect()
}

fn contract_smoke_case_from_object(
    runtime: WorkflowRuntimeKind,
    name: String,
    object: &serde_json::Map<String, JsonValue>,
) -> Result<ContractSmokeCase> {
    let expect_error = expected_error_from_object(object)?;
    let expect_output = object.get("expectOutput").cloned();
    if let Some(command) = object.get("command").and_then(JsonValue::as_str) {
        return non_empty_contract_smoke_command(command).map(|command| ContractSmokeCase {
            name,
            command: ContractSmokeCommand::Shell(command),
            input: object.get("input").cloned(),
            expect_error,
            expect_output,
        });
    }
    let input = object
        .get("input")
        .cloned()
        .unwrap_or_else(|| JsonValue::Object(Default::default()));
    default_contract_smoke_case(runtime, name, input, expect_error, expect_output)
}

fn expected_error_from_object(
    object: &serde_json::Map<String, JsonValue>,
) -> Result<Option<String>> {
    match object.get("expectError") {
        None | Some(JsonValue::Null) | Some(JsonValue::Bool(false)) => Ok(None),
        Some(JsonValue::Bool(true)) => Ok(Some(String::new())),
        Some(JsonValue::String(expected)) if !expected.trim().is_empty() => {
            Ok(Some(expected.trim().to_string()))
        }
        Some(JsonValue::String(_)) => Err(anyhow::anyhow!(
            "validation.contractSmoke.expectError must not be empty"
        )),
        Some(_) => Err(anyhow::anyhow!(
            "validation.contractSmoke.expectError must be a boolean or string"
        )),
    }
}

fn non_empty_contract_smoke_command(command: &str) -> Result<String> {
    let command = command.trim();
    if command.is_empty() {
        return Err(anyhow::anyhow!(
            "validation.contractSmoke.command must not be empty"
        ));
    }
    Ok(command.to_string())
}

fn default_contract_smoke_case(
    runtime: WorkflowRuntimeKind,
    name: String,
    input: JsonValue,
    expect_error: Option<String>,
    expect_output: Option<JsonValue>,
) -> Result<ContractSmokeCase> {
    let command = match runtime {
        WorkflowRuntimeKind::Rune => ContractSmokeCommand::RuneInput(input.clone()),
        WorkflowRuntimeKind::Typescript => {
            ContractSmokeCommand::Shell(default_typescript_contract_smoke_command(&input)?)
        }
    };
    Ok(ContractSmokeCase {
        name,
        command,
        input: Some(input),
        expect_error,
        expect_output,
    })
}

fn default_typescript_contract_smoke_command(input: &JsonValue) -> Result<String> {
    let input_json = serde_json::to_string(input)
        .with_context(|| "failed to serialize validation.contractSmoke.input")?;
    Ok(format!(
        "npm run --silent run -- --input {}",
        shell_quote(&input_json)
    ))
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        return format!("\"{}\"", value.replace('"', "\\\""));
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn contract_smoke_case_command_name(case: &ContractSmokeCase) -> String {
    match &case.command {
        ContractSmokeCommand::Shell(command) => command.clone(),
        ContractSmokeCommand::RuneInput(_) => rune_contract_smoke_command_name(&case.name),
    }
}

fn rune_contract_smoke_command_name(name: &str) -> String {
    if name == "validation.contractSmoke" {
        "Rune validation.contractSmoke".to_string()
    } else {
        format!("Rune validation.contractSmoke ({name})")
    }
}

pub(crate) fn validation_coverage_enabled(spec: &WorkflowSpec, key: &str) -> bool {
    spec.validation
        .get("coverage")
        .and_then(JsonValue::as_object)
        .and_then(|coverage| coverage.get(key))
        == Some(&JsonValue::Bool(true))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn contract_smoke_cases_parse_multiple_cases_and_expected_errors() {
        let spec = crate::spec::WorkflowSpec {
            runtime: Some(crate::spec::WorkflowRuntimeInfo::new(
                crate::spec::WorkflowRuntimeKind::Rune,
                /*entrypoint*/ None,
            )),
            validation: json!({
                "contractSmoke": {
                    "cases": [
                        { "name": "happy", "input": { "value": "ok" } },
                        { "name": "bad input", "input": { "fail": true }, "expectError": "bad input" }
                    ]
                }
            }),
            ..Default::default()
        };

        let cases = super::contract_smoke_cases(&spec).expect("contract smoke cases");

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].name, "happy");
        assert_eq!(cases[1].name, "bad input");
        assert_eq!(cases[1].expect_error, Some("bad input".to_string()));
    }
}
