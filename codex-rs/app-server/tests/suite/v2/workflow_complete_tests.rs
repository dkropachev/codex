use std::fs;
use std::sync::atomic::AtomicBool;

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
use codex_workflows::WorkflowUpdatePolicy;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::workflow_updates::install_local_workflow;

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
