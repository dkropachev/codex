use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::ApiCatalogReadResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn api_catalog_read_returns_methods_tools_and_workflow_runtime() -> Result<()> {
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
            "apiCatalog/read",
            Some(json!({ "mcpDetail": "toolsAndAuthOnly" })),
        )
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await??;
    let response: ApiCatalogReadResponse = to_response(response)?;

    assert_eq!(response.schema_version, 1);
    assert!(response.generated_at > 0);
    assert!(
        response
            .app_server_methods
            .iter()
            .any(|method| method.method == "apiCatalog/read")
    );
    assert!(
        response
            .app_server_methods
            .iter()
            .any(|method| method.method == "mcpServerStatus/list")
    );
    assert_eq!(response.mcp_servers, Vec::new());
    assert_eq!(
        response
            .built_in_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["command/exec", "defineTool", "mcpServer/tool/call"])
    );
    assert_eq!(
        response.workflow_runtime.import_specifier,
        "@openai/codex-sdk/workflow"
    );
    assert!(
        response
            .workflow_runtime
            .symbols
            .iter()
            .any(|symbol| symbol.name == "WorkflowContext.api.read")
    );
    assert!(
        response
            .workflow_runtime
            .symbols
            .iter()
            .any(|symbol| symbol.name == "WorkflowContext.status")
    );
    assert!(
        response
            .app_server_methods
            .iter()
            .any(|method| method.method == "workflow/list")
    );
    assert!(
        response
            .app_server_methods
            .iter()
            .any(|method| method.method == "artifact/state/read")
    );
    assert!(
        response
            .workflow_runtime
            .symbols
            .iter()
            .any(|symbol| symbol.name == "WorkflowContext.artifacts.cache.ensure")
    );
    assert_eq!(response.workflows, Vec::new());

    Ok(())
}
