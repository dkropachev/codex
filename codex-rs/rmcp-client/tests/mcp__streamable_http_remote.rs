//! Integration coverage for the remote Streamable HTTP RMCP path.
//!
//! These tests exercise the orchestrator-side RMCP adapter against a real
//! `exec-server` process so HTTP requests go through the remote runtime path
//! instead of direct local `reqwest` calls.

mod streamable_http_test_support;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use codex_config::types::AuthKeyringBackendKind;
use codex_config::types::OAuthCredentialsStoreMode;
use codex_rmcp_client::ElicitationAction;
use codex_rmcp_client::ElicitationResponse;
use codex_rmcp_client::McpProtocolMode;
use codex_rmcp_client::RmcpClient;
use futures::FutureExt as _;
use pretty_assertions::assert_eq;
use rmcp::model::ClientCapabilities;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::ProtocolVersion;
use serde_json::json;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;

use streamable_http_test_support::call_echo_tool;
use streamable_http_test_support::create_remote_client;
use streamable_http_test_support::expected_echo_result;
use streamable_http_test_support::spawn_exec_server;
use streamable_http_test_support::spawn_streamable_http_server;

/// What this tests: the RMCP remote Streamable HTTP adapter can initialize
/// a server and call a tool while every MCP HTTP request goes through a real
/// exec-server process instead of a direct reqwest transport.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_remote_client_round_trips_through_exec_server() -> anyhow::Result<()> {
    // Phase 1: start the MCP Streamable HTTP test server and a local
    // exec-server process that will own the HTTP network calls.
    let (_server, base_url) = spawn_streamable_http_server().await?;
    let exec_server = spawn_exec_server().await?;

    // Phase 2: create and initialize the RMCP client using the executor-backed
    // Streamable HTTP transport.
    let client = create_remote_client(&base_url, exec_server.client.clone()).await?;

    // Phase 3: prove the initialized client can complete a tool call and
    // preserve the normal RMCP response shape.
    let result = call_echo_tool(&client, "remote").await?;
    assert_eq!(result, expected_echo_result("remote"));

    Ok(())
}

/// A timed-out MCP handshake must release the remote executor for later requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_handshake_timeout_unblocks_remote_executor() -> anyhow::Result<()> {
    stalled_handshake_unblocks_remote_executor(McpProtocolMode::Legacy, "initialize").await
}

/// MCP 2026 discovery must also release the remote executor when it times out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_discovery_timeout_unblocks_remote_executor() -> anyhow::Result<()> {
    stalled_handshake_unblocks_remote_executor(McpProtocolMode::V20260728, "server/discover").await
}

async fn stalled_handshake_unblocks_remote_executor(
    protocol_mode: McpProtocolMode,
    expected_method: &str,
) -> anyhow::Result<()> {
    let http_server = MockServer::start().await;
    let request_started = Arc::new(Notify::new());
    let server_request_started = Arc::clone(&request_started);
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": expected_method })))
        .respond_with(move |_: &wiremock::Request| {
            server_request_started.notify_one();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(/*secs*/ 60))
        })
        .expect(1)
        .mount(&http_server)
        .await;
    let exec_server = spawn_exec_server().await?;
    let client = RmcpClient::new_streamable_http_client_with_protocol_mode(
        "stalled-remote-http",
        &format!("{}/mcp", http_server.uri()),
        Some("test-bearer".to_string()),
        /*http_headers*/ None,
        /*env_http_headers*/ None,
        OAuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
        Arc::new(exec_server.client.clone()),
        /*auth_provider*/ None,
        protocol_mode,
    )
    .await?;
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("codex-test", "0.0.0-test"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_06_18);
    let initialize = client.initialize(
        params,
        Some(Duration::from_secs(/*secs*/ 10)),
        Box::new(|_, _| {
            async {
                Ok(ElicitationResponse {
                    action: ElicitationAction::Accept,
                    content: None,
                    meta: None,
                })
            }
            .boxed()
        }),
    );
    tokio::pin!(initialize);
    tokio::select! {
        result = &mut initialize => anyhow::bail!("handshake finished before stalling: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), request_started.notified()) => {
            result.context("MCP handshake request did not reach the server")?;
        }
    }
    // The real exec-server has its own clock, so let its HTTP deadline expire too.
    let error = initialize
        .await
        .err()
        .context("the stalled MCP handshake must time out")?;
    assert!(
        error.to_string().contains("timed out"),
        "unexpected MCP handshake error: {error}"
    );

    tokio::time::timeout(
        Duration::from_secs(2),
        exec_server.client.environment_status(),
    )
    .await
    .context("a timed-out handshake must not leave the serial executor blocked")??;
    Ok(())
}
