use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

use crate::api_contract::WorkflowSourceContract;
use crate::api_contract::extract_workflow_source_contract_from_typescript;
use crate::api_contract::publish_validated_workflow_api_contract;
use crate::api_contract::read_published_workflow_source_contract;
use crate::api_contract::workflow_api_contract_from_spec_api;
use crate::registry::WorkflowSummary;
use crate::registry::WorkflowValidationStatus;
use crate::registry::discover_workflows;
use crate::validation_runner::WorkflowValidationCommandResult;
use crate::validation_runner::WorkflowValidationReport;
use crate::validation_runner::validate_workflow;
use crate::workflow_client_generation::generate_workflow_client_modules;

pub(crate) fn validate_and_publish_workflow_api<F>(
    codex_home: &Path,
    cwd: &Path,
    config: &codex_config::types::WorkflowsConfigToml,
    workflow: &WorkflowSummary,
    command_runner: F,
) -> Result<WorkflowValidationReport>
where
    F: FnMut(&str, &Path) -> Result<WorkflowValidationCommandResult>,
{
    let mut report = validate_workflow(workflow, command_runner)?;
    if report.status == WorkflowValidationStatus::Valid {
        match resolved_workflow_source_contract(workflow) {
            Ok(source_contract) => {
                let visible_workflows = discover_workflows(codex_home, cwd, config)?;
                let mut source_contracts =
                    published_source_contracts(codex_home, &visible_workflows)?;
                source_contracts.insert(workflow.path.clone(), source_contract.clone());
                generate_workflow_client_modules(&visible_workflows, &source_contracts)?;
                publish_validated_workflow_api_contract(codex_home, workflow, source_contract)?;
            }
            Err(err) => {
                report.status = WorkflowValidationStatus::Invalid;
                report
                    .messages
                    .push(format!("workflow API contract extraction failed: {err}"));
            }
        }
    }
    Ok(report)
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
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "workflow must export WorkflowInput and WorkflowOutput from src/workflow.ts or define api.inputSchema/api.outputSchema in workflow.yaml"
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
                messages: Vec::new(),
            },
            repair_mode: "threshold:3".to_string(),
        }
    }

    fn write_workflow_source(workflow_dir: &Path, source: &str) {
        crate::api_contract::prepare_typescript_workflow_dir(workflow_dir);
        fs::write(workflow_dir.join("src/workflow.ts"), source).expect("workflow ts");
    }

    fn write_minimal_workflow_yaml(workflow_dir: &Path, id: &str, api: serde_json::Value) {
        fs::create_dir_all(workflow_dir).expect("workflow dir");
        crate::spec::write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &crate::spec::WorkflowSpec {
                id: id.to_string(),
                api,
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
        write_workflow_source(&workflow_dir, "export const helper = 1;\n");

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
        write_workflow_source(
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
        );

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
}
