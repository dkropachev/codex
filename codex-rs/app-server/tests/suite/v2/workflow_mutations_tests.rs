use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowDismissParams;
use codex_app_server_protocol::WorkflowDismissResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity as ApiReleaseIdentity;
use codex_app_server_protocol::WorkflowSetPolicyParams;
use codex_app_server_protocol::WorkflowSetPolicyResponse;
use codex_app_server_protocol::WorkflowUninstallParams;
use codex_app_server_protocol::WorkflowUninstallResponse;
use codex_app_server_protocol::WorkflowUpdateParams;
use codex_app_server_protocol::WorkflowUpdatePolicy as ApiUpdatePolicy;
use codex_app_server_protocol::WorkflowUpdateResponse;
use codex_app_server_protocol::WorkflowUpdateStatus;
use codex_app_server_protocol::WorkflowUpdatesChangedNotification;
use codex_app_server_protocol::WorkflowUpdatesReadParams;
use codex_app_server_protocol::WorkflowUpdatesReadResponse;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ManagedWorkflowUpdate;
use codex_workflows::WorkflowReleaseIdentity;
use codex_workflows::WorkflowUpdatePolicy;
use pretty_assertions::assert_eq;
use serde::Serialize;
use tempfile::TempDir;

use super::workflow_updates::commit_tagged_release;
use super::workflow_updates::completed_updates;
use super::workflow_updates::install_local_workflow;

fn api_release(release: &WorkflowReleaseIdentity) -> ApiReleaseIdentity {
    ApiReleaseIdentity {
        tag: release.tag.clone(),
        version: release.version.clone(),
        commit: release.commit.clone(),
    }
}

async fn mutation_error(
    server: &mut TestAppServer,
    method: &str,
    params: impl Serialize,
) -> Result<JSONRPCErrorError> {
    let request_id = server
        .send_request(method, Some(serde_json::to_value(params)?))
        .await?;
    Ok(server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?
        .error)
}

async fn refreshed_snapshot(
    server: &mut TestAppServer,
    previous_scan_id: u64,
) -> Result<WorkflowUpdatesReadResponse> {
    tokio::time::timeout(Duration::from_secs(/*secs*/ 30), async {
        loop {
            let snapshot: WorkflowUpdatesReadResponse = server
                .request(|request_id| ClientRequest::WorkflowUpdatesRead {
                    request_id,
                    params: WorkflowUpdatesReadParams {
                        cursor: None,
                        limit: Some(10),
                    },
                })
                .await?;
            if snapshot.scan_id > previous_scan_id && !snapshot.scanning {
                return Ok::<_, anyhow::Error>(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn workflow_mutations_enforce_release_identity_and_preserve_payload_on_refusal() -> Result<()>
{
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let source = install_local_workflow(
        &service,
        sources.path(),
        "team/build",
        "team-build",
        WorkflowUpdatePolicy::Prompt,
        &cancelled,
    )?;
    let initial = service.list_installed()?[0].installed.clone();
    commit_tagged_release(&source, "v1.1.0")?;
    let available = match service.check_update("team/build", &cancelled)?.update {
        ManagedWorkflowUpdate::Available { release, .. } => release,
        update => anyhow::bail!("expected update, got {update:?}"),
    };
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let mut scan_id = completed_updates(&mut server).await?.scan_id;
    let stale = WorkflowUpdateParams {
        id: "team/build".into(),
        expected_installed: ApiReleaseIdentity {
            commit: "0".repeat(initial.commit.len()),
            ..api_release(&initial)
        },
        expected_available: api_release(&available),
    };
    let error = mutation_error(&mut server, "workflow/update", stale).await?;
    assert!(error.message.contains("release changed before update"));
    assert_eq!(service.list_installed()?[0].installed, initial);
    let error = mutation_error(
        &mut server,
        "workflow/update",
        WorkflowUpdateParams {
            id: "team/build".into(),
            expected_installed: api_release(&initial),
            expected_available: api_release(&initial),
        },
    )
    .await?;
    assert!(error.message.contains("available release changed"));
    assert_eq!(service.list_installed()?[0].installed, initial);
    assert!(
        !home
            .path()
            .join("workflows/team/build/release.txt")
            .exists()
    );

    let updated: WorkflowUpdateResponse = server
        .request(|request_id| ClientRequest::WorkflowUpdate {
            request_id,
            params: WorkflowUpdateParams {
                id: "team/build".into(),
                expected_installed: api_release(&initial),
                expected_available: api_release(&available),
            },
        })
        .await?;
    assert_eq!(updated.installed, api_release(&available));
    assert!(!updated.cleanup_pending);
    assert_eq!(service.list_installed()?[0].installed, available);
    let after_update = refreshed_snapshot(&mut server, scan_id).await?;
    scan_id = after_update.scan_id;
    assert_eq!(after_update.data[0].status, WorkflowUpdateStatus::Current);

    let stale_policy = WorkflowSetPolicyParams {
        id: "team/build".into(),
        expected_installed: api_release(&initial),
        policy: ApiUpdatePolicy::Automatic,
    };
    let error = mutation_error(&mut server, "workflow/setPolicy", stale_policy).await?;
    assert!(
        error
            .message
            .contains("release changed before policy mutation")
    );
    let policy: WorkflowSetPolicyResponse = server
        .request(|request_id| ClientRequest::WorkflowSetPolicy {
            request_id,
            params: WorkflowSetPolicyParams {
                id: "team/build".into(),
                expected_installed: updated.installed.clone(),
                policy: ApiUpdatePolicy::Automatic,
            },
        })
        .await?;
    assert_eq!(policy.managed.policy, ApiUpdatePolicy::Automatic);
    assert_eq!(
        service.list_installed()?[0].policy,
        WorkflowUpdatePolicy::Automatic
    );
    let after_policy = refreshed_snapshot(&mut server, scan_id).await?;
    scan_id = after_policy.scan_id;
    assert_eq!(after_policy.data[0].status, WorkflowUpdateStatus::Current);
    let notified: WorkflowUpdatesChangedNotification =
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
            loop {
                let changed: WorkflowUpdatesChangedNotification =
                    server.read_notification("workflow/updatesChanged").await?;
                if changed.generation == after_policy.generation {
                    return Ok::<_, anyhow::Error>(changed);
                }
            }
        })
        .await??;
    assert_eq!(notified.generation, after_policy.generation);

    commit_tagged_release(&source, "v1.2.0")?;
    let newer = match service.check_update("team/build", &cancelled)?.update {
        ManagedWorkflowUpdate::Available { release, .. } => release,
        update => anyhow::bail!("expected newer update, got {update:?}"),
    };
    let error = mutation_error(
        &mut server,
        "workflow/dismiss",
        WorkflowDismissParams {
            id: "team/build".into(),
            expected_installed: updated.installed.clone(),
            release: api_release(&available),
        },
    )
    .await?;
    assert!(error.message.contains("release changed before dismissal"));
    assert_eq!(service.list_installed()?[0].dismissed_release, None);
    let dismissed: WorkflowDismissResponse = server
        .request(|request_id| ClientRequest::WorkflowDismiss {
            request_id,
            params: WorkflowDismissParams {
                id: "team/build".into(),
                expected_installed: updated.installed.clone(),
                release: api_release(&newer),
            },
        })
        .await?;
    assert_eq!(
        dismissed.managed.dismissed_release,
        Some(api_release(&newer))
    );
    let after_dismissal = refreshed_snapshot(&mut server, scan_id).await?;
    scan_id = after_dismissal.scan_id;
    assert_eq!(
        after_dismissal.data[0].status,
        WorkflowUpdateStatus::Available
    );
    assert!(after_dismissal.data[0].dismissed);
    let explicit: WorkflowUpdateResponse = server
        .request(|request_id| ClientRequest::WorkflowUpdate {
            request_id,
            params: WorkflowUpdateParams {
                id: "team/build".into(),
                expected_installed: updated.installed,
                expected_available: api_release(&newer),
            },
        })
        .await?;
    assert_eq!(explicit.installed, api_release(&newer));
    assert_eq!(service.list_installed()?[0].dismissed_release, None);
    assert_eq!(
        service.list_installed()?[0].policy,
        WorkflowUpdatePolicy::Automatic
    );
    let after_explicit = refreshed_snapshot(&mut server, scan_id).await?;
    scan_id = after_explicit.scan_id;
    assert_eq!(after_explicit.data[0].status, WorkflowUpdateStatus::Current);

    let active = home.path().join("workflows/team/build/release.txt");
    fs::write(&active, "dirty")?;
    let uninstall = WorkflowUninstallParams {
        id: "team/build".into(),
        expected_installed: explicit.installed.clone(),
    };
    let error = mutation_error(&mut server, "workflow/uninstall", uninstall.clone()).await?;
    assert!(
        error
            .message
            .contains("active payload differs from its marker")
    );
    assert_eq!(fs::read_to_string(&active)?, "dirty");
    fs::write(&active, "v1.2.0")?;
    let error = mutation_error(
        &mut server,
        "workflow/uninstall",
        WorkflowUninstallParams {
            expected_installed: api_release(&available),
            ..uninstall.clone()
        },
    )
    .await?;
    assert!(error.message.contains("release changed before uninstall"));
    let removed: WorkflowUninstallResponse = server
        .request(|request_id| ClientRequest::WorkflowUninstall {
            request_id,
            params: uninstall,
        })
        .await?;
    assert!(!removed.cleanup_pending);
    assert_eq!(service.list_installed()?, Vec::new());
    assert!(!active.exists());
    assert_eq!(
        refreshed_snapshot(&mut server, scan_id).await?.data,
        Vec::new()
    );
    Ok(())
}
