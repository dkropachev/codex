use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ManagedWorkflowInfo;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WorkflowListParams;
use codex_app_server_protocol::WorkflowListResponse;
use codex_app_server_protocol::WorkflowReleaseIdentity;
use codex_app_server_protocol::WorkflowSummary;
use codex_app_server_protocol::WorkflowUpdatePolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_workflows::ManagedWorkflowInstallRequest;
use codex_workflows::ManagedWorkflowService;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn workflow_list_paginates_and_respects_project_override() -> Result<()> {
    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let source = scaffold_workflow(
        &project.path().join("sources"),
        &ScaffoldRequest {
            id: "team/build".into(),
            title: "Team Build".into(),
            callable_name: "team-build".into(),
            description: "Build workflow".into(),
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
    let home_path = AbsolutePathBuf::from_absolute_path_checked(home.path())?;
    let service = ManagedWorkflowService::new(&home_path, &home_path.join("workflows"))?;
    let cancelled = AtomicBool::new(false);
    let installed = service.install(ManagedWorkflowInstallRequest {
        source: source
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("source path"))?,
        dependency_runtime: None,
        cancelled: &cancelled,
    })?;
    let project_other = project.path().join(".codex/workflows/team/other");
    fs::create_dir_all(&project_other)?;
    fs::write(
        project_other.join("workflow.yaml"),
        "id: team/other\ncommand: team-other\nuserDescription: Project workflow\n",
    )?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let mut server = TestAppServer::builder()
        .with_codex_home(home.path())
        .build_initialized()
        .await?;
    let cwd = project.path().display().to_string();
    let first: WorkflowListResponse = server
        .request(|request_id| ClientRequest::WorkflowList {
            request_id,
            params: WorkflowListParams {
                cwd: Some(cwd.clone()),
                cursor: None,
                limit: Some(1),
            },
        })
        .await?;
    assert_eq!(
        first,
        WorkflowListResponse {
            data: vec![WorkflowSummary {
                id: "team/build".into(),
                command: "team-build".into(),
                description: "Build workflow".into(),
                path: home
                    .path()
                    .join("workflows/team/build")
                    .display()
                    .to_string(),
                managed: Some(ManagedWorkflowInfo {
                    source: installed.source,
                    installed: WorkflowReleaseIdentity {
                        tag: installed.release.tag,
                        version: installed.release.version.map(|version| version.to_string()),
                        commit: installed.release.advertised_object_id,
                    },
                    policy: WorkflowUpdatePolicy::Prompt,
                    dismissed_release: None,
                }),
            }],
            next_cursor: Some("1".into()),
        }
    );
    let second: WorkflowListResponse = server
        .request(|request_id| ClientRequest::WorkflowList {
            request_id,
            params: WorkflowListParams {
                cwd: Some(cwd.clone()),
                cursor: first.next_cursor,
                limit: Some(1),
            },
        })
        .await?;
    assert_eq!(
        second,
        WorkflowListResponse {
            data: vec![WorkflowSummary {
                id: "team/other".into(),
                command: "team-other".into(),
                description: "Project workflow".into(),
                path: project_other.display().to_string(),
                managed: None,
            }],
            next_cursor: None,
        }
    );
    let project_override = project.path().join(".codex/workflows/team/build");
    fs::create_dir_all(&project_override)?;
    fs::copy(
        source.join("workflow.yaml"),
        project_override.join("workflow.yaml"),
    )?;
    let override_page: WorkflowListResponse = server
        .request(|request_id| ClientRequest::WorkflowList {
            request_id,
            params: WorkflowListParams {
                cwd: Some(cwd),
                cursor: None,
                limit: Some(10),
            },
        })
        .await?;
    assert_eq!(override_page.data[0].managed, None);
    assert_eq!(
        override_page.data[0].path,
        project_override.display().to_string()
    );
    let default_workflow = home.path().join(".codex/workflows/team/default");
    fs::create_dir_all(&default_workflow)?;
    fs::write(
        default_workflow.join("workflow.yaml"),
        "id: team/default\ncommand: team-default\nuserDescription: Default project workflow\n",
    )?;
    let default_page: WorkflowListResponse = server
        .request(|request_id| ClientRequest::WorkflowList {
            request_id,
            params: WorkflowListParams {
                cwd: None,
                cursor: None,
                limit: Some(10),
            },
        })
        .await?;
    assert!(
        default_page
            .data
            .iter()
            .any(|workflow| workflow.id == "team/default")
    );
    fs::write(
        home.path().join("workflows/team/build/workflow.yaml"),
        "id: team/other\ncommand: team-other\nuserDescription: Tampered workflow\n",
    )?;
    let tampered: WorkflowListResponse = server
        .request(|request_id| ClientRequest::WorkflowList {
            request_id,
            params: WorkflowListParams {
                cwd: None,
                cursor: None,
                limit: Some(10),
            },
        })
        .await?;
    let mismatched = tampered
        .data
        .iter()
        .find(|workflow| workflow.id == "team/other")
        .expect("tampered workflow remains discoverable");
    assert_eq!(mismatched.managed, None);
    let request_id = server
        .send_request(
            "workflow/list",
            Some(json!({"cwd": "relative", "limit": 1})),
        )
        .await?;
    let error = server
        .read_stream_until_error_message(RequestId::Integer(request_id))
        .await?;
    assert_eq!(error.error.code, -32602);
    Ok(())
}
