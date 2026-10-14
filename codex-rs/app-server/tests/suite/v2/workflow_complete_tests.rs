use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowCompleteParams;
use codex_app_server_protocol::WorkflowCompleteResponse;
use codex_app_server_protocol::WorkflowCompletionItem;
use codex_app_server_protocol::WorkflowCompletionMode;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ScaffoldRequest;
use codex_workflows::WorkflowUpdatePolicy;
use codex_workflows::scaffold_workflow;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::workflow_updates::install_local_workflow;

#[cfg(unix)]
fn write_slow_bun(tools: &TempDir) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let fake_bun = tools.path().join("bun");
    fs::write(
        &fake_bun,
        "#!/bin/sh\nif [ \"$5\" != complete ]; then exec \"$WORKFLOW_REAL_BUN\" \"$@\"; fi\nprintf x >> \"$WORKFLOW_BUN_MARKER\"\ni=0\nwhile [ ! -f \"$WORKFLOW_BUN_RELEASE\" ] && [ \"$i\" -lt 300 ]; do /bin/sleep 0.1; i=$((i+1)); done\nexit 99\n",
    )?;
    fs::set_permissions(&fake_bun, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[tokio::test]
async fn workflow_completion_rejects_relative_host_cwd() -> Result<()> {
    let home = TempDir::new()?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let request_id = server
        .send_request(
            "workflow/complete",
            Some(serde_json::to_value(WorkflowCompleteParams {
                id: "team/complete".into(),
                cwd: Some("relative".into()),
                input: json!({}),
                active_field: None,
                prefix: String::new(),
                mode: WorkflowCompletionMode::Field,
            })?),
        )
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?;
    assert_eq!(error.error.code, -32602);
    Ok(())
}

#[tokio::test]
async fn managed_completion_reads_published_schema_without_dirtying_install() -> Result<()> {
    if which::which("bun").is_err() {
        return Ok(());
    }
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    install_local_workflow(
        &service,
        sources.path(),
        "team/complete",
        "team-complete",
        WorkflowUpdatePolicy::Prompt,
        &AtomicBool::new(false),
    )?;
    let before = service.list_installed()?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let response: WorkflowCompleteResponse = server
        .request(|request_id| ClientRequest::WorkflowComplete {
            request_id,
            params: WorkflowCompleteParams {
                id: "team/complete".into(),
                cwd: None,
                input: json!({}),
                active_field: None,
                prefix: "--m".into(),
                mode: WorkflowCompletionMode::Field,
            },
        })
        .await?;
    assert_eq!(
        response,
        WorkflowCompleteResponse {
            items: vec![WorkflowCompletionItem {
                value: "--message".into(),
                description: Some("Message to include in the result.".into()),
            }],
            error: None,
        }
    );
    let values: WorkflowCompleteResponse = server
        .request(|request_id| ClientRequest::WorkflowComplete {
            request_id,
            params: WorkflowCompleteParams {
                id: "team/complete".into(),
                cwd: None,
                input: json!({}),
                active_field: Some("message".into()),
                prefix: "--m".into(),
                mode: WorkflowCompletionMode::Value,
            },
        })
        .await?;
    assert_eq!(values.items, Vec::new());
    assert_eq!(values.error, None);
    assert_eq!(service.list_installed()?, before);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn managed_completion_holds_run_lock_and_preserves_installed_payload() -> Result<()> {
    use std::fs::TryLockError;

    let Ok(real_bun) = which::which("bun") else {
        return Ok(());
    };

    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let tools = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    install_local_workflow(
        &service,
        sources.path(),
        "team/complete",
        "team-complete",
        WorkflowUpdatePolicy::Prompt,
        &cancelled,
    )?;
    let before = service.list_installed()?;
    let active = home.path().join("workflows/team/complete/src/workflow.ts");
    let source_before = fs::read(&active)?;
    let marker = tools.path().join("bun-started");
    let release = tools.path().join("bun-release");
    write_slow_bun(&tools)?;
    let path = format!("{}:{}", tools.path().display(), std::env::var("PATH")?);
    let marker_text = marker.display().to_string();
    let release_text = release.display().to_string();
    let real_bun_text = real_bun.display().to_string();
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[
            ("PATH", Some(&path)),
            ("WORKFLOW_BUN_MARKER", Some(&marker_text)),
            ("WORKFLOW_BUN_RELEASE", Some(&release_text)),
            ("WORKFLOW_REAL_BUN", Some(&real_bun_text)),
        ])
        .build_initialized()
        .await?;
    let params = WorkflowCompleteParams {
        id: "team/complete".into(),
        cwd: None,
        input: json!({}),
        active_field: None,
        prefix: "--".into(),
        mode: WorkflowCompletionMode::Field,
    };
    let request_id = server
        .send_request("workflow/complete", Some(serde_json::to_value(&params)?))
        .await?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while !marker.is_file() {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?;
    let lock_file = fs::OpenOptions::new().read(true).write(true).open(
        home.path()
            .join(".workflow-management/locks/team/complete.lock"),
    )?;
    assert!(matches!(
        lock_file.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    fs::write(&release, "")?;
    let response: WorkflowCompleteResponse = server.read_response(request_id).await?;
    assert!(response.error.is_some());
    lock_file.try_lock()?;
    assert_eq!(service.list_installed()?, before);
    assert_eq!(fs::read(&active)?, source_before);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn completion_limits_concurrent_developer_jobs() -> Result<()> {
    let Ok(real_bun) = which::which("bun") else {
        return Ok(());
    };
    let home = TempDir::new()?;
    let tools = TempDir::new()?;
    scaffold_workflow(
        &home.path().join("workflows"),
        &ScaffoldRequest {
            id: "team/developer".into(),
            title: "Developer".into(),
            callable_name: "team-developer".into(),
            description: "Developer workflow".into(),
        },
    )?;
    write_slow_bun(&tools)?;
    let marker = tools.path().join("bun-started");
    let release = tools.path().join("bun-release");
    let path = format!("{}:{}", tools.path().display(), std::env::var("PATH")?);
    let marker_text = marker.display().to_string();
    let release_text = release.display().to_string();
    let real_bun_text = real_bun.display().to_string();
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .with_env_overrides(&[
            ("PATH", Some(&path)),
            ("WORKFLOW_BUN_MARKER", Some(&marker_text)),
            ("WORKFLOW_BUN_RELEASE", Some(&release_text)),
            ("WORKFLOW_REAL_BUN", Some(&real_bun_text)),
        ])
        .build_initialized()
        .await?;
    let params = WorkflowCompleteParams {
        id: "team/developer".into(),
        cwd: None,
        input: json!({}),
        active_field: None,
        prefix: "--".into(),
        mode: WorkflowCompletionMode::Field,
    };
    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            server
                .send_request("workflow/complete", Some(serde_json::to_value(&params)?))
                .await?,
        );
    }
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while fs::metadata(&marker).map_or(0, |metadata| metadata.len()) < 2 {
            tokio::time::sleep(Duration::from_millis(/*millis*/ 20)).await;
        }
    })
    .await?;
    let rejected_id = server
        .send_request("workflow/complete", Some(serde_json::to_value(&params)?))
        .await?;
    let overloaded = server
        .read_stream_until_error_message(RequestId::Integer(rejected_id))
        .await?;
    assert_eq!(overloaded.error.code, -32001);
    fs::write(&release, "")?;
    for id in ids {
        let response: WorkflowCompleteResponse = server.read_response(id).await?;
        assert!(response.error.is_some());
    }
    Ok(())
}

#[tokio::test]
async fn project_override_completion_uses_developer_package() -> Result<()> {
    if which::which("bun").is_err() {
        return Ok(());
    }
    let home = TempDir::new()?;
    let sources = TempDir::new()?;
    let project = TempDir::new()?;
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    install_local_workflow(
        &service,
        sources.path(),
        "team/complete",
        "team-complete",
        WorkflowUpdatePolicy::Prompt,
        &AtomicBool::new(false),
    )?;
    let before = service.list_installed()?;
    let active = home.path().join("workflows/team/complete/src/workflow.ts");
    fs::write(&active, "modified managed payload")?;
    let override_root = scaffold_workflow(
        &project.path().join(".codex/workflows"),
        &ScaffoldRequest {
            id: "team/complete".into(),
            title: "Project Complete".into(),
            callable_name: "team-complete".into(),
            description: "Project override".into(),
        },
    )?;
    let module = override_root.join("src/workflow.ts");
    let source = fs::read_to_string(&module)?.replace(
        "Message to include in the result.",
        "Project override message.",
    );
    fs::write(&module, source)?;
    for args in [
        &["add", "--all"][..],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "override",
        ][..],
    ] {
        anyhow::ensure!(
            Command::new("git")
                .current_dir(&override_root)
                .args(args)
                .status()?
                .success()
        );
    }
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let response: WorkflowCompleteResponse = server
        .request(|request_id| ClientRequest::WorkflowComplete {
            request_id,
            params: WorkflowCompleteParams {
                id: "team/complete".into(),
                cwd: Some(project.path().display().to_string()),
                input: json!({}),
                active_field: None,
                prefix: "--m".into(),
                mode: WorkflowCompletionMode::Field,
            },
        })
        .await?;
    assert_eq!(
        response.items,
        vec![WorkflowCompletionItem {
            value: "--message".into(),
            description: Some("Project override message.".into()),
        }]
    );
    assert_eq!(response.error, None);
    assert_eq!(service.list_installed()?, before);
    assert_eq!(fs::read_to_string(&active)?, "modified managed payload");
    Ok(())
}
