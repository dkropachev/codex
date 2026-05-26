use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowDevelopResponse;
use codex_app_server_protocol::WorkflowDiscardResponse;
use codex_app_server_protocol::WorkflowEditResponse;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowPublishResponse;
use codex_app_server_protocol::WorkflowReadResponse;
use codex_app_server_protocol::WorkflowRepairActionKind;
use codex_app_server_protocol::WorkflowRepairResponse;
use codex_app_server_protocol::WorkflowRepairStopReason;
use codex_app_server_protocol::WorkflowRunResponse;
use codex_app_server_protocol::WorkflowRuntimeKind;
use codex_app_server_protocol::WorkflowValidateResponse;
use codex_app_server_protocol::WorkflowValidationFindingInfo;
use codex_app_server_protocol::WorkflowValidationStatus;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

fn append_workflows_config(codex_home: &TempDir, extra: &str) -> Result<()> {
    fs::OpenOptions::new()
        .append(true)
        .open(codex_home.path().join("config.toml"))?
        .write_all(extra.as_bytes())?;
    Ok(())
}

fn write_valid_workflow(
    workflow_dir: &Path,
    id: &str,
    title: &str,
    description: &str,
) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join(".gitignore"),
        "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {id}\ntitle: {title}\nuserDescription: {description}\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        format!(
            "# {title}\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        format!(
            "# {title} Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("package.json"),
        format!(
            "{{\n  \"name\": \"codex-workflow-{}\",\n  \"private\": true,\n  \"type\": \"module\"\n}}\n",
            id.replace('/', "-")
        ),
    )?;
    fs::write(
        workflow_dir.join(".gitignore"),
        "artifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport default async function runWorkflow(_ctx: unknown, _input: WorkflowInput): Promise<WorkflowOutput> {\n  return { ok: true };\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    Ok(())
}

fn write_valid_rune_workflow(
    workflow_dir: &Path,
    id: &str,
    title: &str,
    description: &str,
) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join(".gitignore"),
        "artifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {id}\nruntime:\n  kind: rune\n  entrypoint: src/workflow.rn\ntitle: {title}\nuserDescription: {description}\napi:\n  callableName: runeReport\n  inputSchema:\n    type: object\n    additionalProperties: true\n  outputSchema:\n    type: object\n    properties:\n      ok:\n        type: boolean\n      input:\n        type: object\n        additionalProperties: true\n    required:\n      - ok\n      - input\n    additionalProperties: false\nvalidation:\n  commands:\n    - \"true\"\n  contractSmoke:\n    input: {{}}\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        format!(
            "# {title}\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        format!(
            "# {title} Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        ),
    )?;
    fs::write(
        workflow_dir.join("src/workflow.rn"),
        "pub async fn run(_ctx, input) { #{ ok: true, input } }\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.rn"),
        "// workflow-covers: positive progress finalResult\npub fn covers_positive_progress_final_result() {\n    true\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.rn"),
        "// workflow-covers: load\npub fn covers_load() {\n    true\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.rn"),
        "// workflow-covers: autocomplete\npub fn covers_autocomplete() {\n    true\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.rn"),
        "// workflow-covers: negative failureUx\npub fn covers_negative_failure_ux() {\n    true\n}\n",
    )?;
    fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    Ok(())
}

fn write_broken_repair_fixture(workflow_dir: &Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: broken/other\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        "# Broken\n\n## Usage\n\n## Workflow Runtime\n",
    )?;
    fs::write(workflow_dir.join("DESIGN.md"), "# Broken Design\n")?;
    fs::write(
        workflow_dir.join("package.json"),
        "{\n  \"name\": \"broken\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.ts"),
        "import leftPad from \"left-pad\";\nimport { WorkflowContext } from \"@openai/codex-sdk/workflow\";\n\nexport interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; input: WorkflowInput; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function run(_ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true, input: { input: leftPad(input.input ?? \"\", 2) } }; }\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    Ok(())
}

fn write_unsupported_command_fixture(workflow_dir: &Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: broken/fix\nvalidation:\n  commands:\n    - node -e \"console.log('out'); console.error('err'); process.exit(1)\"\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        "# Workflow\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Workflow Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
    )?;
    fs::write(
        workflow_dir.join("package.json"),
        "{\n  \"name\": \"codex-workflow-failing-command\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    )?;
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function workflow() { return { ok: true }; }\nexport async function complete() { return []; }\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )?;
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )?;
    fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    Ok(())
}

fn write_schema_repair_fixture(
    workflow_dir: &Path,
    id: &str,
    api: serde_json::Value,
    tool: Option<codex_workflows::WorkflowToolSpec>,
) -> Result<()> {
    write_valid_workflow(workflow_dir, id, "Schema Repair", "Repair schema metadata")?;
    codex_workflows::write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &codex_workflows::WorkflowSpec {
            id: id.to_string(),
            title: Some("Schema Repair".to_string()),
            user_description: Some("Repair schema metadata".to_string()),
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
    )?;
    Ok(())
}

#[tokio::test]
async fn workflow_develop_defaults_to_rune_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/develop",
            Some(json!({ "description": "Rune Report" })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowDevelopResponse = to_response(response)?;
    assert_eq!(response.data["id"], "rune-report");

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "rune-report" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let read: WorkflowReadResponse = to_response(response)?;

    assert_eq!(read.workflow.runtime.kind, WorkflowRuntimeKind::Rune);
    assert_eq!(read.workflow.runtime.entrypoint, "src/workflow.rn");
    assert!(
        codex_home
            .path()
            .join("workflows/rune-report/src/workflow.rn")
            .is_file()
    );
    assert!(
        !codex_home
            .path()
            .join("workflows/rune-report/package.json")
            .exists()
    );

    Ok(())
}

#[tokio::test]
async fn workflow_run_executes_rune_workflow_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/rune/report");
    write_valid_rune_workflow(
        &workflow_dir,
        "rune/report",
        "Rune Report",
        "Run a Rune workflow",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/run",
            Some(json!({
                "id": "rune/report",
                "input": { "value": "ok" }
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRunResponse = to_response(response)?;
    let stdout = response.data["stdout"].as_str().expect("stdout string");
    let output: serde_json::Value = serde_json::from_str(stdout)?;

    assert_eq!(
        output,
        json!({
            "ok": true,
            "input": { "value": "ok" }
        })
    );
    assert_eq!(response.data["stderr"], "");

    Ok(())
}

#[tokio::test]
async fn workflow_validate_rejects_invalid_rune_source_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/rune/broken");
    write_valid_rune_workflow(
        &workflow_dir,
        "rune/broken",
        "Broken Rune",
        "Validate a broken Rune workflow",
    )?;
    fs::write(
        workflow_dir.join("src/workflow.rn"),
        "pub async fn run(_ctx, input) { let = input }\n",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/validate", Some(json!({ "id": "rune/broken" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowValidateResponse = to_response(response)?;
    let findings = response.data["validation"]["findings"]
        .as_array()
        .expect("validation findings");

    assert_eq!(response.data["validation"]["status"], "invalid");
    assert!(
        response
            .message
            .contains("failed to compile workflow runtime source")
    );
    assert!(findings.iter().any(|finding| {
        finding["type"] == "workflowRuntimeCompileFailed" && finding["path"] == "src/workflow.rn"
    }));

    Ok(())
}

#[tokio::test]
async fn workflow_list_returns_discovered_workflows() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/reports/jira-summary");
    write_valid_workflow(
        &workflow_dir,
        "reports/jira-summary",
        "Jira Summary",
        "Summarize Jira work",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/list", Some(json!({})))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowListResponse = to_response(response)?;

    assert_eq!(response.workflows.len(), 1);
    assert_eq!(response.workflows[0].id, "reports/jira-summary");
    assert_eq!(
        response.workflows[0].title,
        Some("Jira Summary".to_string())
    );
    assert_eq!(
        response.workflows[0].validation.status,
        WorkflowValidationStatus::Valid
    );

    Ok(())
}

#[tokio::test]
async fn workflow_repair_returns_structured_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(&codex_home, "\n[workflows]\ncommit_policy = \"manual\"\n")?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_broken_repair_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert!(response.message.contains("Repairing workflow"));
    assert!(response.message.contains("Validation passed."));
    assert_eq!(response.repair.stop_reason, WorkflowRepairStopReason::Valid);
    assert!(response.repair.changed);
    assert!(!response.repair.applied_fixes.is_empty());
    assert_eq!(response.validation.status, WorkflowValidationStatus::Valid);
    assert!(response.validation.findings.is_empty());

    Ok(())
}

#[tokio::test]
async fn workflow_repair_returns_blocked_mode_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\nrepair_mode = \"metadata\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_broken_repair_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.message.contains("Blocked findings:"));
    assert!(!response.repair.blocked_findings.is_empty());
    assert!(response.repair.unsupported_findings.is_empty());

    Ok(())
}

#[tokio::test]
async fn workflow_repair_returns_unsupported_command_result() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(&codex_home, "\n[workflows]\ncommit_policy = \"manual\"\n")?;
    let workflow_dir = codex_home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir)?;
    write_unsupported_command_fixture(&workflow_dir)?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::UnsupportedFindings
    );
    assert!(response.message.contains("Unsupported findings:"));
    assert!(!response.repair.unsupported_findings.is_empty());
    assert!(!response.repair.changed);
    assert_eq!(response.validation_command_results.len(), 1);
    assert!(
        response.validation_command_results[0]
            .stdout
            .contains("out")
    );
    assert!(
        response.validation_command_results[0]
            .stderr
            .contains("err")
    );

    Ok(())
}

#[tokio::test]
async fn workflow_repair_repairs_missing_design_and_schema_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(&codex_home, "\n[workflows]\ncommit_policy = \"manual\"\n")?;
    let workflow_dir = codex_home.path().join("workflows/broken/schema");
    write_schema_repair_fixture(
        &workflow_dir,
        "broken/schema",
        json!({
            "inputSchema": { "type": "object", "additionalProperties": true },
            "outputSchema": {
                "type": "object",
                "properties": {
                    "nested": { "type": "object" }
                }
            }
        }),
        Some(codex_workflows::WorkflowToolSpec {
            description: "Run broken/schema".to_string(),
            input_schema: json!({ "type": "object", "additionalProperties": true }),
            output_schema: json!({ "type": "object" }),
            ..Default::default()
        }),
    )?;
    fs::remove_file(workflow_dir.join("DESIGN.md"))?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/schema" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(response.repair.stop_reason, WorkflowRepairStopReason::Valid);
    assert!(response.repair.changed);
    assert_eq!(response.validation.status, WorkflowValidationStatus::Valid);
    assert!(response.validation.findings.is_empty());
    assert!(
        response
            .repair
            .applied_fixes
            .iter()
            .any(|fix| { fix.kind == WorkflowRepairActionKind::RepairDesign })
    );
    assert!(
        response
            .repair
            .applied_fixes
            .iter()
            .any(|fix| { fix.kind == WorkflowRepairActionKind::NormalizeValidationMetadata })
    );
    assert!(workflow_dir.join("DESIGN.md").is_file());

    let spec = codex_workflows::read_workflow_spec(&workflow_dir.join("workflow.yaml"))?;
    assert_eq!(
        spec.api["outputSchema"]["properties"]["nested"]["additionalProperties"],
        true
    );
    assert_eq!(
        spec.tool.unwrap().output_schema["additionalProperties"],
        true
    );

    Ok(())
}

#[tokio::test]
async fn workflow_repair_blocked_schema_finding_round_trips_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\nrepair_mode = \"none\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/schema");
    write_schema_repair_fixture(
        &workflow_dir,
        "broken/schema",
        json!({
            "inputSchema": { "type": "object", "additionalProperties": true },
            "outputSchema": { "type": "object" }
        }),
        /*tool*/ None,
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request("workflow/repair", Some(json!({ "id": "broken/schema" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.repair.blocked_findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFindingInfo::AmbiguousWorkflowOutputSchema { schema_path, .. }
                if schema_path == "api.outputSchema"
        )
    }));

    Ok(())
}

#[tokio::test]
async fn workflow_repair_blocked_runtime_state_finding_round_trips_e2e() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    append_workflows_config(
        &codex_home,
        "\n[workflows]\ncommit_policy = \"manual\"\nrepair_mode = \"metadata\"\n",
    )?;
    let workflow_dir = codex_home.path().join("workflows/broken/runtime-state");
    write_valid_workflow(
        &workflow_dir,
        "broken/runtime-state",
        "Runtime State",
        "Repair runtime state metadata",
    )?;
    fs::remove_file(workflow_dir.join(".gitignore"))?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/repair",
            Some(json!({ "id": "broken/runtime-state" })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: WorkflowRepairResponse = to_response(response)?;

    assert_eq!(
        response.repair.stop_reason,
        WorkflowRepairStopReason::BlockedByRepairMode
    );
    assert!(response.repair.blocked_findings.iter().any(|finding| {
        matches!(
            finding,
            WorkflowValidationFindingInfo::RuntimeStateGitignoreMissing { patterns, .. }
                if patterns.iter().any(|pattern| pattern == "state/*")
        )
    }));

    Ok(())
}

#[tokio::test]
async fn workflow_stage_session_id_keeps_edits_private_until_done() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/review/fix");
    write_valid_workflow(
        &workflow_dir,
        "review/fix",
        "Review Fix",
        "Repair a workflow",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/edit",
            Some(json!({
                "id": "review/fix",
                "instruction": "staged note",
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowEditResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "review/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let live_before_done: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        live_before_done
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        false
    );

    let request_id = mcp
        .send_raw_request(
            "workflow/read",
            Some(json!({
                "id": "review/fix",
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let staged_read: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        staged_read
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        true
    );

    let request_id = mcp
        .send_raw_request(
            "workflow/publish",
            Some(json!({
                "stageSessionId": "session-123"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowPublishResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request("workflow/read", Some(json!({ "id": "review/fix" })))
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let live_after_done: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        live_after_done
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("staged note")),
        true
    );

    Ok(())
}

#[tokio::test]
async fn workflow_stage_session_id_discard_removes_staged_changes() -> Result<()> {
    let server = create_mock_responses_server_sequence_unchecked(Vec::new()).await;
    let codex_home = TempDir::new()?;
    write_mock_responses_config_toml(
        codex_home.path(),
        &server.uri(),
        &BTreeMap::new(),
        /*auto_compact_limit*/ 1024,
        /*requires_openai_auth*/ None,
        "mock_provider",
        "compact",
    )?;
    let workflow_dir = codex_home.path().join("workflows/review/fix");
    write_valid_workflow(
        &workflow_dir,
        "review/fix",
        "Review Fix",
        "Repair a workflow",
    )?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let request_id = mcp
        .send_raw_request(
            "workflow/edit",
            Some(json!({
                "id": "review/fix",
                "instruction": "discarded note",
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowEditResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request(
            "workflow/discard",
            Some(json!({
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let _: WorkflowDiscardResponse = to_response(response)?;

    let request_id = mcp
        .send_raw_request(
            "workflow/read",
            Some(json!({
                "id": "review/fix",
                "stageSessionId": "session-456"
            })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let read_after_discard: WorkflowReadResponse = to_response(response)?;
    assert_eq!(
        read_after_discard
            .readme
            .as_deref()
            .is_some_and(|readme| readme.contains("discarded note")),
        false
    );

    Ok(())
}
