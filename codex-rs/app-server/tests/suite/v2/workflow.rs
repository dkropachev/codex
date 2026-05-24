use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowRepairResponse;
use codex_app_server_protocol::WorkflowRepairStopReason;
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

fn write_broken_repair_fixture(workflow_dir: &std::path::Path) -> Result<()> {
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

fn write_unsupported_command_fixture(workflow_dir: &std::path::Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: broken/fix\nvalidation:\n  commands:\n    - exit 1\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
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
    fs::create_dir_all(workflow_dir.join("src/tests"))?;
    fs::create_dir_all(workflow_dir.join("state"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: reports/jira-summary\ntitle: Jira Summary\nuserDescription: Summarize Jira work\nvalidation:\n  commands:\n    - npm run build\n    - npm test\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
    )?;
    fs::write(
        workflow_dir.join("README.md"),
        "# Jira Summary\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
    )?;
    fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Jira Summary Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
    )?;
    fs::write(
        workflow_dir.join("package.json"),
        "{\n  \"name\": \"codex-workflow-reports-jira-summary\",\n  \"private\": true,\n  \"type\": \"module\"\n}\n",
    )?;
    fs::write(workflow_dir.join("src/workflow.ts"), "export {};\n")?;
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

    assert_eq!(response.message, "valid");
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
    assert!(!response.repair.unsupported_findings.is_empty());
    assert!(!response.repair.changed);

    Ok(())
}
