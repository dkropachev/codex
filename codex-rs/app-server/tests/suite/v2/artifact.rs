use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use anyhow::Result;
use app_test_support::McpProcess;
use app_test_support::create_mock_responses_server_sequence_unchecked;
use app_test_support::to_response;
use app_test_support::write_mock_responses_config_toml;
use codex_app_server_protocol::ArtifactCacheDeleteResponse;
use codex_app_server_protocol::ArtifactCacheReadResponse;
use codex_app_server_protocol::ArtifactCacheWriteResponse;
use codex_app_server_protocol::ArtifactFileFindResponse;
use codex_app_server_protocol::ArtifactFileIndexResponse;
use codex_app_server_protocol::ArtifactStateListResponse;
use codex_app_server_protocol::ArtifactStatePruneResponse;
use codex_app_server_protocol::ArtifactStateReadResponse;
use codex_app_server_protocol::ArtifactStateRegisterResponse;
use codex_app_server_protocol::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn artifact_rpc_round_trip_uses_the_shared_store() -> Result<()> {
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

    let state_dir = codex_home.path().join("artifacts/workflow-state");
    fs::create_dir_all(&state_dir)?;
    fs::write(state_dir.join("report.txt"), "artifact contents\n")?;

    let mut mcp = McpProcess::new(codex_home.path()).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;

    let register = to_response::<ArtifactStateRegisterResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/state/register",
                json!({
                    "namespace": "workflow",
                    "scopeKey": "reports/jira",
                    "sourceKey": "reports/jira:state",
                    "stateDir": state_dir,
                    "sources": [{
                        "path": "report.txt",
                        "kind": "report",
                        "sha256": "abc123"
                    }],
                    "metadata": { "revision": 1, "expires": false }
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(register.state.namespace, "workflow");
    assert_eq!(register.state.scope_key, "reports/jira");
    assert_eq!(
        register.state.metadata,
        json!({ "revision": 1, "expires": false })
    );

    let read = to_response::<ArtifactStateReadResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/state/read",
                json!({
                    "namespace": "workflow",
                    "scopeKey": "reports/jira",
                    "sourceKey": "reports/jira:state"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(read.state, Some(register.state.clone()));

    let list = to_response::<ArtifactStateListResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/state/list",
                json!({
                    "namespace": "workflow",
                    "scopeKey": "reports/jira"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(list.states, vec![register.state.clone()]);

    let hit = response_for(
        &mut mcp,
        "artifact/state/hit",
        json!({
            "namespace": "workflow",
            "stateDir": state_dir
        }),
    )
    .await?;
    assert_eq!(hit, json!({}));

    let prune = to_response::<ArtifactStatePruneResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/state/prune",
                json!({
                    "namespace": "workflow",
                    "retentionSecs": 3600,
                    "throttleSecs": 0
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(prune.pruned, 0);

    let index = to_response::<ArtifactFileIndexResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/file/index",
                json!({
                    "namespace": "workflow",
                    "stateDir": state_dir,
                    "relativePath": "report.txt"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(index.file.state_id, register.state.id);
    assert_eq!(
        index.file.relative_path,
        std::path::PathBuf::from("report.txt")
    );

    let find = to_response::<ArtifactFileFindResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/file/find",
                json!({
                    "namespace": "workflow",
                    "relativePath": "report.txt"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(
        find.entry.as_ref().map(|entry| entry.state.id),
        Some(register.state.id)
    );
    assert_eq!(
        find.entry
            .as_ref()
            .map(|entry| entry.file.relative_path.as_path()),
        Some(std::path::Path::new("report.txt"))
    );

    let cache_write = to_response::<ArtifactCacheWriteResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/cache/write",
                json!({
                    "namespace": "workflow",
                    "key": "reports/jira",
                    "artifactId": "reports/jira:state",
                    "status": "fresh",
                    "metadata": { "refreshAfter": 60 }
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(cache_write.entry.namespace, "workflow");
    assert_eq!(cache_write.entry.metadata, json!({ "refreshAfter": 60 }));

    let cache_read = to_response::<ArtifactCacheReadResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/cache/read",
                json!({
                    "namespace": "workflow",
                    "key": "reports/jira"
                }),
            ),
        )
        .await??,
    )?;
    let cache_read = cache_read.entry.expect("cache entry should exist");
    assert_eq!(cache_read.namespace, cache_write.entry.namespace);
    assert_eq!(cache_read.key, cache_write.entry.key);
    assert_eq!(cache_read.artifact_id, cache_write.entry.artifact_id);
    assert_eq!(cache_read.status, cache_write.entry.status);
    assert_eq!(cache_read.metadata, cache_write.entry.metadata);

    let delete = to_response::<ArtifactCacheDeleteResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/cache/delete",
                json!({
                    "namespace": "workflow",
                    "key": "reports/jira"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(delete, ArtifactCacheDeleteResponse {});

    let cache_read_after_delete = to_response::<ArtifactCacheReadResponse>(
        timeout(
            DEFAULT_READ_TIMEOUT,
            response_for(
                &mut mcp,
                "artifact/cache/read",
                json!({
                    "namespace": "workflow",
                    "key": "reports/jira"
                }),
            ),
        )
        .await??,
    )?;
    assert_eq!(cache_read_after_delete.entry, None);

    Ok(())
}

async fn response_for(
    mcp: &mut McpProcess,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let request_id = mcp.send_raw_request(method, Some(params)).await?;
    let response = mcp
        .read_stream_until_response_message(RequestId::Integer(request_id))
        .await?;
    Ok(response)
}
