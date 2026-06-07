use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;

use crate::api_contract::WorkflowSourceContract;
use crate::api_contract::extract_workflow_source_contract_from_typescript;
use crate::api_contract::publish_validated_workflow_api_contract;
use crate::api_contract::read_published_workflow_source_contract;
use crate::api_contract::workflow_api_contract_from_spec_api;
use crate::registry::WorkflowSummary;
use crate::registry::WorkflowValidationStatus;
use crate::registry::discover_workflows;
use crate::spec::WorkflowSpec;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_runner::WorkflowValidationCommandResult;
use crate::validation_runner::WorkflowValidationReport;
use crate::validation_runner::validate_workflow;
use crate::workflow_client_generation::generate_workflow_client_modules;
use crate::workflow_contract_validation::validate_json_against_schema;

pub(crate) fn validate_and_publish_workflow_api<F>(
    codex_home: &Path,
    cwd: &Path,
    config: &codex_config::types::WorkflowsConfigToml,
    workflow: &WorkflowSummary,
    mut command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let (report, source_contract) =
        validate_workflow_api_contract_inner(workflow, &mut command_runner)?;
    if report.status == WorkflowValidationStatus::Valid
        && let Some(source_contract) = source_contract
    {
        let visible_workflows = discover_workflows(codex_home, cwd, config)?;
        let mut source_contracts = published_source_contracts(codex_home, &visible_workflows)?;
        source_contracts.insert(workflow.path.clone(), source_contract.clone());
        generate_workflow_client_modules(&visible_workflows, &source_contracts)?;
        publish_validated_workflow_api_contract(codex_home, workflow, source_contract)?;
    }
    Ok(report)
}

pub(crate) fn validate_workflow_api_contract<F>(
    workflow: &WorkflowSummary,
    command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    validate_workflow_api_contract_inner(workflow, command_runner).map(|(report, _)| report)
}

fn validate_workflow_api_contract_inner<F>(
    workflow: &WorkflowSummary,
    mut command_runner: F,
) -> Result<(WorkflowValidationReport, Option<WorkflowSourceContract>)>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let mut report = validate_workflow(workflow, &mut command_runner)?;
    if report.status != WorkflowValidationStatus::Valid {
        return Ok((report, None));
    }

    let source_contract = match resolved_workflow_source_contract(workflow) {
        Ok(source_contract) => source_contract,
        Err(err) => {
            report.push_finding(
                WorkflowValidationFinding::WorkflowApiContractExtractionFailed {
                    path: workflow.path.join("src/workflow.ts"),
                    error: err.to_string(),
                },
            );
            return Ok((report, None));
        }
    };

    let spec = crate::spec::read_workflow_spec(&workflow.workflow_yaml_path)?;
    match contract_smoke_command(&spec) {
        Ok(Some(command)) => run_contract_smoke(
            workflow,
            &source_contract,
            &command,
            &mut command_runner,
            &mut report,
        )?,
        Ok(None) => {}
        Err(err) => {
            report.push_finding(WorkflowValidationFinding::WorkflowApiContractSmokeFailed {
                command: "validation.contractSmoke".to_string(),
                error: err.to_string(),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    let source_contract =
        (report.status == WorkflowValidationStatus::Valid).then_some(source_contract);
    Ok((report, source_contract))
}

fn run_contract_smoke<F>(
    workflow: &WorkflowSummary,
    source_contract: &WorkflowSourceContract,
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

fn contract_smoke_command(spec: &WorkflowSpec) -> Result<Option<String>> {
    let Some(smoke) = spec.validation.get("contractSmoke") else {
        return Ok(None);
    };

    match smoke {
        JsonValue::Null | JsonValue::Bool(false) => Ok(None),
        JsonValue::Bool(true) => {
            default_contract_smoke_command(&JsonValue::Object(Default::default())).map(Some)
        }
        JsonValue::String(command) => non_empty_contract_smoke_command(command),
        JsonValue::Object(object) => {
            if object.get("enabled") == Some(&JsonValue::Bool(false)) {
                return Ok(None);
            }
            if let Some(command) = object.get("command").and_then(JsonValue::as_str) {
                return non_empty_contract_smoke_command(command);
            }
            let input = object
                .get("input")
                .cloned()
                .unwrap_or_else(|| JsonValue::Object(Default::default()));
            default_contract_smoke_command(&input).map(Some)
        }
        _ => Err(anyhow!(
            "validation.contractSmoke must be false, true, a command string, or an object"
        )),
    }
}

fn non_empty_contract_smoke_command(command: &str) -> Result<Option<String>> {
    let command = command.trim();
    if command.is_empty() {
        return Err(anyhow!(
            "validation.contractSmoke.command must not be empty"
        ));
    }
    Ok(Some(command.to_string()))
}

fn default_contract_smoke_command(input: &JsonValue) -> Result<String> {
    let input_json = serde_json::to_string(input)
        .with_context(|| "failed to serialize validation.contractSmoke.input")?;
    let input_literal = serde_json::to_string(&input_json)
        .with_context(|| "failed to serialize validation.contractSmoke.input literal")?;
    let source = format!(
        r#"const mod=await import("./src/workflow.ts");if(typeof mod.default!=="function"){{throw new Error("workflow default export must be a function");}}const input=JSON.parse({input_literal});const ctx={{progress(){{}},reportToUserMarkdown(){{}},status(){{}},cwd:process.cwd(),currentWorkingDirectory:process.cwd(),repoRoot:process.cwd(),workingDirectory:process.cwd()}};const output=await mod.default(ctx,input);console.log(JSON.stringify(output));"#
    );
    Ok(format!("bun --eval {}", shell_quote(&source)))
}

fn shell_quote(value: &str) -> String {
    if cfg!(windows) {
        return format!("\"{}\"", value.replace('"', "\\\""));
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(crate) fn resolved_workflow_source_contract(
    workflow: &WorkflowSummary,
) -> Result<WorkflowSourceContract> {
    let spec = crate::spec::read_workflow_spec(&workflow.workflow_yaml_path)?;
    let source_has_default_export_function =
        workflow_source_contains_default_export_function(&workflow.path)?;

    let extracted = match extract_workflow_source_contract_from_typescript(&workflow.path) {
        Ok(extracted) => extracted,
        Err(err) => {
            if !source_has_default_export_function
                && let Some(contract) = workflow_api_contract_from_spec_api(&spec.api)
            {
                return Ok(WorkflowSourceContract {
                    callable_name: None,
                    input_schema: contract.input_schema,
                    output_schema: contract.output_schema,
                    format_schemas: contract.format_schemas,
                    hooks: contract.hooks,
                });
            }
            return Err(err);
        }
    };

    let extracted_has_contract = !extracted.input_schema.is_null()
        || !extracted.output_schema.is_null()
        || !extracted.format_schemas.is_empty();
    if extracted_has_contract {
        if extracted.input_schema.is_null() {
            anyhow::bail!(
                "export WorkflowInput from src/workflow.ts when using TS-defined workflow contracts"
            );
        }
        if extracted.output_schema.is_null() {
            anyhow::bail!(
                "export WorkflowOutput from src/workflow.ts when using TS-defined workflow contracts"
            );
        }
        return Ok(extracted);
    }

    if !source_has_default_export_function {
        return workflow_api_contract_from_spec_api(&spec.api)
            .map(|contract| WorkflowSourceContract {
                callable_name: None,
                input_schema: contract.input_schema,
                output_schema: contract.output_schema,
                format_schemas: contract.format_schemas,
                hooks: contract.hooks,
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "workflow must export WorkflowInput and WorkflowOutput from src/workflow.ts"
                )
            });
    }

    anyhow::bail!(
        "workflow default export must be a named async function with ctx and input parameters"
    )
}

fn published_source_contracts(
    codex_home: &Path,
    workflows: &[WorkflowSummary],
) -> Result<BTreeMap<PathBuf, WorkflowSourceContract>> {
    let mut contracts = BTreeMap::new();
    for workflow in workflows {
        if let Some(contract) = read_published_workflow_source_contract(codex_home, workflow)? {
            contracts.insert(workflow.path.clone(), contract);
        }
    }
    Ok(contracts)
}

fn workflow_source_contains_default_export_function(workflow_dir: &Path) -> Result<bool> {
    let workflow_path = workflow_dir.join("src/workflow.ts");
    let contents = fs::read_to_string(&workflow_path)
        .with_context(|| format!("failed to read workflow source {}", workflow_path.display()))?;
    Ok(contents.contains("export default function")
        || contents.contains("export default async function")
        || contents.contains("export default (")
        || contents.contains("export default async ("))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;

    use super::resolved_workflow_source_contract;
    use super::validate_and_publish_workflow_api;
    use crate::api_contract::WorkflowSourceContract;
    use crate::api_contract::publish_validated_workflow_api_contract;
    use crate::api_contract::read_published_workflow_source_contract;
    use crate::registry::WorkflowRootKind;
    use crate::registry::WorkflowSummary;
    use crate::registry::WorkflowValidation;
    use crate::registry::WorkflowValidationStatus;
    use crate::validation_runner::WorkflowValidationCommandResult;

    fn workflow_summary(
        root_label: &str,
        root_kind: WorkflowRootKind,
        root_path: &Path,
        workflow_dir: &Path,
        id: &str,
    ) -> WorkflowSummary {
        WorkflowSummary {
            id: id.to_string(),
            command: Some(id.split('/').next_back().unwrap_or(id).to_string()),
            title: Some(id.to_string()),
            user_description: Some(id.to_string()),
            search_terms: Vec::new(),
            command_option_hints: Vec::new(),
            root_label: root_label.to_string(),
            root_kind,
            root_path: root_path.to_path_buf(),
            path: workflow_dir.to_path_buf(),
            workflow_yaml_path: workflow_dir.join("workflow.yaml"),
            mention_target: format!("workflow:///tmp#{id}"),
            validation: WorkflowValidation {
                status: WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        }
    }

    fn write_workflow_source(workflow_dir: &Path, source: &str) -> bool {
        if !crate::api_contract::prepare_typescript_workflow_dir(workflow_dir) {
            return false;
        }
        fs::write(workflow_dir.join("src/workflow.ts"), source).expect("workflow ts");
        true
    }

    fn write_minimal_workflow_yaml(workflow_dir: &Path, id: &str, api: serde_json::Value) {
        write_workflow_yaml(workflow_dir, id, api, serde_json::Value::Null);
    }

    fn write_workflow_yaml(
        workflow_dir: &Path,
        id: &str,
        api: serde_json::Value,
        validation: serde_json::Value,
    ) {
        fs::create_dir_all(workflow_dir).expect("workflow dir");
        crate::spec::write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: id.to_string(),
                api,
                validation,
                ..Default::default()
            },
        )
        .expect("workflow yaml");
    }

    fn success_result(command: &str) -> WorkflowValidationCommandResult {
        WorkflowValidationCommandResult {
            command: command.to_string(),
            succeeded: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn resolved_workflow_source_contract_falls_back_to_workflow_yaml_api_for_legacy_sources() {
        let workflow_root = TempDir::new().expect("workflow root");
        let workflow_dir = workflow_root.path().join("legacy/report");
        write_minimal_workflow_yaml(
            &workflow_dir,
            "legacy/report",
            json!({
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "workflowId": { "type": "string" }
                    },
                    "required": ["workflowId"],
                    "additionalProperties": false
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string" }
                    },
                    "required": ["status"],
                    "additionalProperties": false
                },
                "formatSchemas": {
                    "tui.markdown.v1": {
                        "type": "object",
                        "properties": {
                            "markdown": { "type": "string" }
                        },
                        "required": ["markdown"],
                        "additionalProperties": false
                    }
                }
            }),
        );
        if !write_workflow_source(&workflow_dir, "export const helper = 1;\n") {
            return;
        }

        let workflow = workflow_summary(
            "global",
            WorkflowRootKind::Global,
            workflow_root.path(),
            &workflow_dir,
            "legacy/report",
        );

        let contract = resolved_workflow_source_contract(&workflow).expect("legacy fallback");
        assert_eq!(
            contract,
            WorkflowSourceContract {
                callable_name: None,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "workflowId": { "type": "string" }
                    },
                    "required": ["workflowId"],
                    "additionalProperties": false
                }),
                output_schema: json!({
                    "type": "object",
                    "properties": {
                        "status": { "type": "string" }
                    },
                    "required": ["status"],
                    "additionalProperties": false
                }),
                format_schemas: BTreeMap::from([(
                    "tui.markdown.v1".to_string(),
                    json!({
                        "type": "object",
                        "properties": {
                            "markdown": { "type": "string" }
                        },
                        "required": ["markdown"],
                        "additionalProperties": false
                    }),
                )]),
                hooks: Default::default(),
            }
        );
    }

    #[test]
    fn validate_and_publish_workflow_api_keeps_previous_contract_when_generation_fails() {
        let codex_home = TempDir::new().expect("codex home");
        let cwd = TempDir::new().expect("cwd");
        let config = codex_config::types::WorkflowsConfigToml::default();

        let shared_workflow_dir = codex_home.path().join("workflows/review/shared");
        write_minimal_workflow_yaml(&shared_workflow_dir, "review/shared", json!({}));
        if !write_workflow_source(
            &shared_workflow_dir,
            r#"
export interface WorkflowInput {
  value: string;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.status };
  },
};

export default async function sharedReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.value };
}
"#,
        ) {
            return;
        }

        let shared_workflow = workflow_summary(
            "global",
            WorkflowRootKind::Global,
            &codex_home.path().join("workflows"),
            &shared_workflow_dir,
            "review/shared",
        );

        let current_workflow_dir = cwd.path().join(".codex/workflows/review/current");
        write_minimal_workflow_yaml(&current_workflow_dir, "review/current", json!({}));
        write_workflow_source(
            &current_workflow_dir,
            r#"
export interface WorkflowInput {
  value: string;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.status };
  },
};

export default async function sharedReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.value };
}
"#,
        );

        let current_workflow = workflow_summary(
            "project",
            WorkflowRootKind::Project,
            &cwd.path().join(".codex/workflows"),
            &current_workflow_dir,
            "review/current",
        );

        let shared_contract = WorkflowSourceContract {
            callable_name: Some("sharedReview".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" }
                },
                "required": ["status"],
                "additionalProperties": false
            }),
            format_schemas: BTreeMap::from([(
                "tui.markdown.v1".to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "markdown": { "type": "string" }
                    },
                    "required": ["markdown"],
                    "additionalProperties": false
                }),
            )]),
            hooks: Default::default(),
        };
        let current_contract = WorkflowSourceContract {
            callable_name: Some("oldReview".to_string()),
            ..shared_contract.clone()
        };

        publish_validated_workflow_api_contract(
            codex_home.path(),
            &shared_workflow,
            shared_contract,
        )
        .expect("publish shared workflow contract");
        publish_validated_workflow_api_contract(
            codex_home.path(),
            &current_workflow,
            current_contract.clone(),
        )
        .expect("publish current workflow contract");

        let err = validate_and_publish_workflow_api(
            codex_home.path(),
            cwd.path(),
            &config,
            &current_workflow,
            |command, _cwd| Ok(success_result(command)),
        )
        .expect_err("duplicate callable names should fail generation");
        assert!(
            err.to_string()
                .contains("duplicate workflow callable name `sharedReview`")
        );

        let published =
            read_published_workflow_source_contract(codex_home.path(), &current_workflow)
                .expect("read current published contract")
                .expect("expected current published contract to remain intact");
        assert_eq!(published, current_contract);
    }

    #[test]
    fn validate_and_publish_workflow_api_rejects_contract_smoke_output_mismatch() {
        let codex_home = TempDir::new().expect("codex home");
        let cwd = TempDir::new().expect("cwd");
        let config = codex_config::types::WorkflowsConfigToml::default();

        let workflow_dir = codex_home.path().join("workflows/review/smoke");
        write_workflow_yaml(
            &workflow_dir,
            "review/smoke",
            json!({}),
            json!({
                "commands": ["bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk", "bun test src/tests"],
                "contractSmoke": {
                    "command": "bun src/workflow.ts --input '{\"value\":\"ok\"}'"
                }
            }),
        );
        if !write_workflow_source(
            &workflow_dir,
            r#"
export interface WorkflowInput {
  value: string;
}

export interface WorkflowOutput {
  status: string;
}

export default async function smokeReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.value };
}
"#,
        ) {
            return;
        }

        let workflow = workflow_summary(
            "global",
            WorkflowRootKind::Global,
            &codex_home.path().join("workflows"),
            &workflow_dir,
            "review/smoke",
        );

        let report = validate_and_publish_workflow_api(
            codex_home.path(),
            cwd.path(),
            &config,
            &workflow,
            |command, _cwd| {
                if command.starts_with("bun src/workflow.ts") {
                    Ok(WorkflowValidationCommandResult {
                        command: command.to_string(),
                        succeeded: true,
                        exit_code: Some(0),
                        stdout: r#"{"status":"ok","reviewId":"extra"}"#.to_string(),
                        stderr: String::new(),
                    })
                } else {
                    Ok(success_result(command))
                }
            },
        )
        .expect("workflow API validation should complete");

        assert_eq!(report.status, WorkflowValidationStatus::Invalid);
        assert_eq!(report.command_results.len(), 3);
        assert_eq!(
            crate::validation_finding::finding_messages(&report.findings),
            vec![
                "workflow contract smoke command `bun src/workflow.ts --input '{\"value\":\"ok\"}'` failed: output did not match the workflow contract: workflow contract violation at $.reviewId: unexpected property".to_string()
            ]
        );
        assert_eq!(
            read_published_workflow_source_contract(codex_home.path(), &workflow)
                .expect("read published workflow contract"),
            None
        );
    }

    #[test]
    fn validate_and_publish_workflow_api_publishes_after_contract_smoke_passes() {
        let codex_home = TempDir::new().expect("codex home");
        let cwd = TempDir::new().expect("cwd");
        let config = codex_config::types::WorkflowsConfigToml::default();

        let workflow_dir = codex_home.path().join("workflows/review/smoke-ok");
        write_workflow_yaml(
            &workflow_dir,
            "review/smoke-ok",
            json!({}),
            json!({
                "commands": ["bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk", "bun test src/tests"],
                "contractSmoke": { "input": { "value": "ok" } }
            }),
        );
        if !write_workflow_source(
            &workflow_dir,
            r#"
export interface WorkflowInput {
  value: string;
}

export interface WorkflowOutput {
  status: string;
}

export default async function smokeOkReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.value };
}
"#,
        ) {
            return;
        }

        let workflow = workflow_summary(
            "global",
            WorkflowRootKind::Global,
            &codex_home.path().join("workflows"),
            &workflow_dir,
            "review/smoke-ok",
        );

        let report = validate_and_publish_workflow_api(
            codex_home.path(),
            cwd.path(),
            &config,
            &workflow,
            |command, _cwd| {
                if command.starts_with("bun --eval") {
                    Ok(WorkflowValidationCommandResult {
                        command: command.to_string(),
                        succeeded: true,
                        exit_code: Some(0),
                        stdout: r#"{"status":"ok"}"#.to_string(),
                        stderr: String::new(),
                    })
                } else {
                    Ok(success_result(command))
                }
            },
        )
        .expect("workflow API validation should pass");

        assert_eq!(report.status, WorkflowValidationStatus::Valid);
        assert_eq!(report.command_results.len(), 3);
        assert_eq!(
            report.command_results[2].command,
            super::default_contract_smoke_command(&json!({ "value": "ok" })).unwrap()
        );
        assert!(
            read_published_workflow_source_contract(codex_home.path(), &workflow)
                .expect("read published workflow contract")
                .is_some()
        );
    }
}
