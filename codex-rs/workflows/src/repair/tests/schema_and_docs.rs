use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;

use crate::execute::WorkflowCommandContext;
use crate::execute::WorkflowCommandOutput;
use crate::spec::WorkflowSpec;
use crate::spec::WorkflowToolSpec;
use crate::spec::read_workflow_spec;
use crate::spec::write_workflow_spec;

fn write_repairable_workflow_fixture(
    workflow_dir: &Path,
    id: &str,
    api: JsonValue,
    tool: Option<WorkflowToolSpec>,
) {
    super::write_command_failure_workflow_fixture(workflow_dir);
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &WorkflowSpec {
            id: id.to_string(),
            api,
            tool,
            validation: json!({
                "commands": ["exit 0"],
                "coverage": {
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }
            }),
            ..Default::default()
        },
    )
    .unwrap();
}

fn run_repair(home: &TempDir, cwd: &TempDir, id: &str) -> WorkflowCommandOutput {
    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
        runtime_event_handler: None,
        runtime: Default::default(),
    };

    super::super::repair_workflow_command(ctx, id).unwrap()
}

#[test]
fn repair_workflow_command_recreates_missing_design_doc() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/no-design");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_repairable_workflow_fixture(
        &workflow_dir,
        "broken/no-design",
        JsonValue::Null,
        /*tool*/ None,
    );
    fs::remove_file(workflow_dir.join("DESIGN.md")).unwrap();

    let output = run_repair(&home, &cwd, "broken/no-design");

    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["repair"]["changed"], true);
    assert_eq!(output.data["validation"]["findings"], json!([]));
    assert!(workflow_dir.join("DESIGN.md").is_file());
    let design = fs::read_to_string(workflow_dir.join("DESIGN.md")).unwrap();
    assert!(design.contains("## Architecture"));
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| fixes.iter().any(|fix| fix["kind"] == "repairDesign"))
    );
}

#[test]
fn repair_workflow_command_normalizes_ambiguous_output_schemas() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/schema");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_repairable_workflow_fixture(
        &workflow_dir,
        "broken/schema",
        json!({
            "inputSchema": { "type": "object", "additionalProperties": true },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "nested": { "type": "object" },
                    "choice": {
                        "oneOf": [
                            { "type": "object" },
                            { "type": "string" }
                        ]
                    },
                    "items": {
                        "type": "array",
                        "items": { "type": "object" }
                    }
                }
            }
        }),
        Some(WorkflowToolSpec {
            description: "Run broken/schema".to_string(),
            input_schema: json!({ "type": "object", "additionalProperties": true }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "nested": { "type": "object" },
                    "choice": {
                        "oneOf": [
                            { "type": "object" },
                            { "type": "string" }
                        ]
                    },
                    "items": {
                        "type": "array",
                        "items": { "type": "object" }
                    }
                }
            }),
            ..Default::default()
        }),
    );

    let output = run_repair(&home, &cwd, "broken/schema");

    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["validation"]["findings"], json!([]));
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| fixes
                .iter()
                .any(|fix| fix["kind"] == "normalizeValidationMetadata"))
    );

    let spec = read_workflow_spec(&workflow_dir.join("workflow.yaml")).unwrap();
    assert_eq!(spec.api, JsonValue::Null);
    let tool = spec.tool.unwrap();
    assert_eq!(
        tool.output_schema["properties"]["nested"]["additionalProperties"],
        true
    );
    assert_eq!(
        tool.output_schema["properties"]["choice"]["oneOf"][0]["additionalProperties"],
        true
    );
    assert_eq!(
        tool.output_schema["properties"]["items"]["items"]["additionalProperties"],
        true
    );
}
