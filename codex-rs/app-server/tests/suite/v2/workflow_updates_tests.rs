use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::to_response;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::WorkflowCheckUpdatesParams;
use codex_app_server_protocol::WorkflowCheckUpdatesResponse;
use codex_app_server_protocol::WorkflowUpdateStatus;
use codex_app_server_protocol::WorkflowUpdatesChangedNotification;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowInstallRequest;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::connection_handling_websocket::WsClient;
use super::connection_handling_websocket::connect_websocket;
use super::connection_handling_websocket::read_response_for_id;
use super::connection_handling_websocket::send_request;
use super::connection_handling_websocket::spawn_websocket_server;

fn install_local_workflow(
    service: &ManagedWorkflowService,
    sources: &Path,
    id: &str,
    command: &str,
    cancelled: &AtomicBool,
) -> Result<PathBuf> {
    let source = scaffold_workflow(
        sources,
        &ScaffoldRequest {
            id: id.into(),
            title: command.into(),
            callable_name: command.into(),
            description: command.into(),
        },
    )?;
    for arguments in [
        &["add", "--all"][..],
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"][..],
    ] {
        let status = Command::new("git")
            .current_dir(&source)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(arguments)
            .status()?;
        anyhow::ensure!(status.success(), "Git command failed: {arguments:?}");
    }
    service.install(ManagedWorkflowInstallRequest {
        source: source
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("source path"))?,
        dependency_runtime: None,
        cancelled,
    })?;
    Ok(source)
}

async fn completed_websocket_updates(
    client: &mut WsClient,
    next_id: &mut i64,
) -> Result<WorkflowUpdatesReadResponse> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let id = *next_id;
            *next_id += 1;
            send_request(client, "workflow/updatesRead", id, Some(json!({}))).await?;
            let response: WorkflowUpdatesReadResponse =
                to_response(read_response_for_id(client, id).await?)?;
            if !response.scanning {
                return Ok::<_, anyhow::Error>(response);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?
}

async fn completed_updates(server: &mut TestAppServer) -> Result<WorkflowUpdatesReadResponse> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let response: WorkflowUpdatesReadResponse = server
                .request(|request_id| ClientRequest::WorkflowUpdatesRead {
                    request_id,
                    params: WorkflowUpdatesReadParams {
                        cursor: None,
                        limit: Some(1),
                    },
                })
                .await?;
            if !response.scanning {
                return Ok::<_, anyhow::Error>(response);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn workflow_updates_snapshot_survives_restart_and_reports_local_source_error() -> Result<()> {
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let build_source = install_local_workflow(
        &service,
        sources.path(),
        "team/build",
        "team-build",
        &cancelled,
    )?;
    let moved_source = install_local_workflow(
        &service,
        sources.path(),
        "team/check",
        "team-check",
        &cancelled,
    )?;
    fs::rename(moved_source, sources.path().join("moved-check"))?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let run_workspace = service.prepare_run_workspace(
        "team/build",
        &home.path().join("workflows/team/build"),
        &cancelled,
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let mut waiting = Box::pin(
        server.request(|request_id| ClientRequest::WorkflowUpdatesRead {
            request_id,
            params: WorkflowUpdatesReadParams {
                cursor: None,
                limit: Some(1),
            },
        }),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(/*millis*/ 100), &mut waiting)
            .await
            .is_err()
    );
    drop(run_workspace);
    let _: WorkflowUpdatesReadResponse =
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), waiting).await??;
    let first = completed_updates(&mut server).await?;
    assert_eq!(first.generation, 4);
    assert_eq!(first.error, None);
    assert_eq!(first.data[0].id, "team/build");
    assert_eq!(first.data[0].status, WorkflowUpdateStatus::Current);
    assert_eq!(first.next_cursor, Some("1".into()));
    let second: WorkflowUpdatesReadResponse = server
        .request(|request_id| ClientRequest::WorkflowUpdatesRead {
            request_id,
            params: WorkflowUpdatesReadParams {
                cursor: first.next_cursor.clone(),
                limit: Some(1),
            },
        })
        .await?;
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.data[0].id, "team/check");
    assert_eq!(second.data[0].status, WorkflowUpdateStatus::Error);
    assert!(
        second.data[0]
            .error
            .as_ref()
            .is_some_and(|error| !error.is_empty())
    );
    assert_eq!(second.next_cursor, None);
    fs::rename(build_source, sources.path().join("moved-build"))?;
    let run_workspace = service.prepare_run_workspace(
        "team/build",
        &home.path().join("workflows/team/build"),
        &cancelled,
    )?;
    let refresh: WorkflowCheckUpdatesResponse = server
        .request(|request_id| ClientRequest::WorkflowCheckUpdates {
            request_id,
            params: WorkflowCheckUpdatesParams {},
        })
        .await?;
    let duplicate: WorkflowCheckUpdatesResponse = server
        .request(|request_id| ClientRequest::WorkflowCheckUpdates {
            request_id,
            params: WorkflowCheckUpdatesParams {},
        })
        .await?;
    assert!(refresh.started);
    assert_eq!(duplicate.scan_id, refresh.scan_id);
    assert!(!duplicate.started);
    let pending: WorkflowUpdatesReadResponse = server
        .request(|request_id| ClientRequest::WorkflowUpdatesRead {
            request_id,
            params: WorkflowUpdatesReadParams {
                cursor: None,
                limit: Some(1),
            },
        })
        .await?;
    assert_eq!(pending.scan_id, refresh.scan_id);
    assert!(pending.scanning);
    let changed = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            let changed: WorkflowUpdatesChangedNotification =
                server.read_notification("workflow/updatesChanged").await?;
            if changed.generation == pending.generation {
                return Ok::<_, anyhow::Error>(changed);
            }
        }
    })
    .await??;
    assert_eq!(changed.generation, pending.generation);
    drop(run_workspace);
    let refreshed = completed_updates(&mut server).await?;
    assert_eq!(refreshed.scan_id, refresh.scan_id);
    assert!(refreshed.generation > pending.generation);
    assert_eq!(refreshed.data[0].status, WorkflowUpdateStatus::Error);
    let final_changed = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        loop {
            let changed: WorkflowUpdatesChangedNotification =
                server.read_notification("workflow/updatesChanged").await?;
            if changed.generation == refreshed.generation {
                return Ok::<_, anyhow::Error>(changed);
            }
        }
    })
    .await??;
    assert_eq!(final_changed.generation, refreshed.generation);
    drop(server);

    let mut restarted = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let restarted_snapshot = completed_updates(&mut restarted).await?;
    assert_eq!(restarted_snapshot.scan_id, 1);
    assert_eq!(restarted_snapshot.data, refreshed.data);
    assert_eq!(service.list_installed()?.len(), 2);
    Ok(())
}

#[tokio::test]
async fn workflow_updates_snapshot_is_retained_for_a_reconnected_client() -> Result<()> {
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    install_local_workflow(
        &service,
        sources.path(),
        "team/reconnect",
        "team-reconnect",
        &AtomicBool::new(false),
    )?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let (mut process, address) = spawn_websocket_server(home.path()).await?;
    let initialize = serde_json::to_value(InitializeParams {
        client_info: ClientInfo {
            name: "workflow_snapshot_test".into(),
            title: None,
            version: "0.1.0".into(),
        },
        capabilities: Some(InitializeCapabilities {
            experimental_api: true,
            ..Default::default()
        }),
    })?;
    let mut first_client = connect_websocket(address).await?;
    send_request(
        &mut first_client,
        "initialize",
        /*id*/ 1,
        Some(initialize.clone()),
    )
    .await?;
    read_response_for_id(&mut first_client, /*id*/ 1).await?;
    let mut next_id = 2;
    let startup = completed_websocket_updates(&mut first_client, &mut next_id).await?;
    assert_eq!(startup.data[0].id, "team/reconnect");
    let refresh_id = next_id;
    next_id += 1;
    send_request(
        &mut first_client,
        "workflow/checkUpdates",
        refresh_id,
        Some(json!({})),
    )
    .await?;
    let refresh: WorkflowCheckUpdatesResponse =
        to_response(read_response_for_id(&mut first_client, refresh_id).await?)?;
    assert!(refresh.started);
    let first = completed_websocket_updates(&mut first_client, &mut next_id).await?;
    assert_eq!(first.scan_id, refresh.scan_id);
    assert!(first.generation > startup.generation);
    assert_eq!(first.data, startup.data);
    drop(first_client);

    let mut reconnected = connect_websocket(address).await?;
    send_request(
        &mut reconnected,
        "initialize",
        /*id*/ 3,
        Some(initialize),
    )
    .await?;
    read_response_for_id(&mut reconnected, /*id*/ 3).await?;
    let retained = completed_websocket_updates(&mut reconnected, &mut next_id).await?;
    assert_eq!(retained, first);
    process.kill().await?;
    Ok(())
}
