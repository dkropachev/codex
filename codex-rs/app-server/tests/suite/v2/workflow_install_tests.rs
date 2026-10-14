use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowInstallParams;
use codex_app_server_protocol::WorkflowInstallResponse;
use codex_app_server_protocol::WorkflowResolveSourceParams;
use codex_app_server_protocol::WorkflowResolveSourceResponse;
use codex_app_server_protocol::WorkflowUpdatePolicy as ApiUpdatePolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::WorkflowUpdatePolicy;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::workflow_updates::commit_tagged_release;
use super::workflow_updates::install_local_workflow;

#[tokio::test]
async fn workflow_install_requires_selected_release_and_persists_requested_policy() -> Result<()> {
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let source = install_local_workflow(
        &service,
        sources.path(),
        "team/install",
        "team-install",
        WorkflowUpdatePolicy::Prompt,
        &cancelled,
    )?;
    let initial = service.list_installed()?[0].installed.clone();
    service.uninstall("team/install", &initial, &cancelled)?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let source_text = format!("./{}", source.strip_prefix(sources.path())?.display());
    let source_cwd = Some(sources.path().display().to_string());
    let selected: WorkflowResolveSourceResponse = server
        .request(|request_id| ClientRequest::WorkflowResolveSource {
            request_id,
            params: WorkflowResolveSourceParams {
                source: source_text.clone(),
                cwd: source_cwd.clone(),
            },
        })
        .await?;
    assert_eq!(selected.release.commit, initial.commit);
    commit_tagged_release(&source, "v1.1.0")?;
    let stale = WorkflowInstallParams {
        source: source_text.clone(),
        cwd: source_cwd.clone(),
        expected_release: selected.release,
        policy: ApiUpdatePolicy::Manual,
    };
    let request_id = server
        .send_request("workflow/install", Some(serde_json::to_value(&stale)?))
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?;
    assert!(
        error
            .error
            .message
            .contains("release changed before install")
    );
    assert_eq!(service.list_installed()?, Vec::new());

    let current: WorkflowResolveSourceResponse = server
        .request(|request_id| ClientRequest::WorkflowResolveSource {
            request_id,
            params: WorkflowResolveSourceParams {
                source: source_text.clone(),
                cwd: source_cwd.clone(),
            },
        })
        .await?;
    let installed: WorkflowInstallResponse = server
        .request(|request_id| ClientRequest::WorkflowInstall {
            request_id,
            params: WorkflowInstallParams {
                source: source_text.clone(),
                cwd: source_cwd.clone(),
                expected_release: current.release.clone(),
                policy: ApiUpdatePolicy::Manual,
            },
        })
        .await?;
    assert_eq!(installed.id, "team/install");
    assert_eq!(installed.installed, current.release);
    assert!(!installed.cleanup_pending);
    assert_eq!(
        service.list_installed()?[0].policy,
        WorkflowUpdatePolicy::Manual
    );
    assert_eq!(
        fs::read_to_string(home.path().join("workflows/team/install/release.txt"))?,
        "v1.1.0"
    );
    let before = service.list_installed()?;
    let before_payload =
        fs::read_to_string(home.path().join("workflows/team/install/release.txt"))?;
    let request_id = server
        .send_request(
            "workflow/install",
            Some(serde_json::to_value(WorkflowInstallParams {
                source: source_text,
                cwd: source_cwd,
                expected_release: installed.installed,
                policy: ApiUpdatePolicy::Prompt,
            })?),
        )
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?;
    assert!(
        error
            .error
            .message
            .contains("receipt changed before publication")
    );
    assert_eq!(service.list_installed()?, before);
    assert_eq!(
        fs::read_to_string(home.path().join("workflows/team/install/release.txt"))?,
        before_payload
    );
    Ok(())
}

#[tokio::test]
async fn workflow_install_resolves_untagged_file_url_on_server_host() -> Result<()> {
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let source = install_local_workflow(
        &service,
        sources.path(),
        "team/head",
        "team-head",
        WorkflowUpdatePolicy::Prompt,
        &cancelled,
    )?;
    let initial = service.list_installed()?[0].installed.clone();
    service.uninstall("team/head", &initial, &cancelled)?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let source_url = url::Url::from_file_path(&source)
        .map_err(|()| anyhow::anyhow!("source file URL"))?
        .to_string();
    let canonical_url = url::Url::from_file_path(source.canonicalize()?)
        .map_err(|()| anyhow::anyhow!("canonical source file URL"))?
        .to_string();
    let selected: WorkflowResolveSourceResponse = server
        .request(|request_id| ClientRequest::WorkflowResolveSource {
            request_id,
            params: WorkflowResolveSourceParams {
                source: source_url.clone(),
                cwd: None,
            },
        })
        .await?;
    assert_eq!(selected.release.tag, None);
    assert_eq!(selected.release.version, None);
    assert_eq!(selected.release.commit, initial.commit);
    let installed: WorkflowInstallResponse = server
        .request(|request_id| ClientRequest::WorkflowInstall {
            request_id,
            params: WorkflowInstallParams {
                source: source_url.clone(),
                cwd: None,
                expected_release: selected.release.clone(),
                policy: ApiUpdatePolicy::Prompt,
            },
        })
        .await?;
    assert_eq!(installed.installed, selected.release);
    let record = &service.list_installed()?[0];
    assert_eq!(record.source, canonical_url);
    assert_eq!(record.policy, WorkflowUpdatePolicy::Prompt);
    assert!(
        home.path()
            .join("workflows/team/head/workflow.yaml")
            .is_file()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn startup_scan_and_source_requests_share_four_git_slots() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let tools = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let source = install_local_workflow(
        &service,
        sources.path(),
        "team/limit",
        "team-limit",
        WorkflowUpdatePolicy::Prompt,
        &cancelled,
    )?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let real_git = which::which("git")?;
    let marker = tools.path().join("git-started");
    let release = tools.path().join("git-release");
    let wrapper = tools.path().join("git");
    fs::write(
        &wrapper,
        "#!/bin/sh\nprintf x >> \"$WORKFLOW_GIT_MARKER\"\ni=0\nwhile [ ! -f \"$WORKFLOW_GIT_RELEASE\" ] && [ \"$i\" -lt 300 ]; do /bin/sleep 0.1; i=$((i+1)); done\nexec \"$WORKFLOW_REAL_GIT\" \"$@\"\n",
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    let path = format!("{}:{}", tools.path().display(), std::env::var("PATH")?);
    let marker_text = marker.display().to_string();
    let release_text = release.display().to_string();
    let real_git_text = real_git.display().to_string();
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[
            ("PATH", Some(&path)),
            ("WORKFLOW_GIT_MARKER", Some(&marker_text)),
            ("WORKFLOW_GIT_RELEASE", Some(&release_text)),
            ("WORKFLOW_REAL_GIT", Some(&real_git_text)),
        ])
        .build_initialized()
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while fs::metadata(&marker).is_err() {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?;
    let source_text = source.display().to_string();
    let mut request_ids = Vec::new();
    for _ in 0..3 {
        request_ids.push(
            server
                .send_request(
                    "workflow/resolveSource",
                    Some(serde_json::to_value(WorkflowResolveSourceParams {
                        source: source_text.clone(),
                        cwd: None,
                    })?),
                )
                .await?,
        );
    }
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while fs::metadata(&marker).map_or(0, |metadata| metadata.len()) < 4 {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?;
    let rejected_id = server
        .send_request(
            "workflow/resolveSource",
            Some(serde_json::to_value(WorkflowResolveSourceParams {
                source: source_text,
                cwd: None,
            })?),
        )
        .await?;
    let rejected = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        server.read_stream_until_error_message(RequestId::Integer(rejected_id)),
    )
    .await?;
    fs::write(&release, "")?;
    let rejected = rejected?;
    assert_eq!(rejected.error.code, -32001);
    for request_id in request_ids {
        let _: WorkflowResolveSourceResponse = server.read_response(request_id).await?;
    }
    Ok(())
}
