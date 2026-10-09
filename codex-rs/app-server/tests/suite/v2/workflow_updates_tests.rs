use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::WorkflowUpdateStatus;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowInstallRequest;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

async fn completed_updates(server: &mut TestAppServer) -> Result<WorkflowUpdatesReadResponse> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
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
    let mut moved_source = None;
    for (id, command) in [("team/build", "team-build"), ("team/check", "team-check")] {
        let source = scaffold_workflow(
            sources.path(),
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
            cancelled: &cancelled,
        })?;
        if id == "team/check" {
            moved_source = Some(source);
        }
    }
    fs::rename(
        moved_source.expect("check source"),
        sources.path().join("moved-check"),
    )?;
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
    drop(server);

    let mut restarted = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    assert_eq!(completed_updates(&mut restarted).await?, first);
    assert_eq!(service.list_installed()?.len(), 2);
    Ok(())
}
