use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowValidationStatus;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

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
    fs::create_dir_all(workflow_dir.join("src"))?;
    fs::create_dir_all(workflow_dir.join(".git"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: reports/jira-summary\ntitle: Jira Summary\nuserDescription: Summarize Jira work\n",
    )?;
    fs::write(workflow_dir.join("README.md"), "# Jira Summary\n")?;
    fs::write(workflow_dir.join("src/workflow.ts"), "export {};\n")?;

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
