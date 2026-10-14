use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use codex_app_server_client::AppServerClient;
use codex_workflows::CompletionItem;
use codex_workflows::CompletionMode;
use codex_workflows::CompletionRequest;
use codex_workflows::CompletionResult;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::WorkflowCompletionTask;
use super::spawn_completion;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;

#[tokio::test]
async fn completion_uses_app_server_project_workflow() -> color_eyre::Result<()> {
    if std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_err()
    {
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let workflow_dir = scaffold_workflow(
        &project.path().join(".codex/workflows"),
        &ScaffoldRequest {
            id: "team/complete".into(),
            title: "Complete".into(),
            callable_name: "team-complete".into(),
            description: "Completion fixture".into(),
        },
    )
    .map_err(|error| color_eyre::eyre::eyre!("{error}"))?;
    std::fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(home.path().to_path_buf()),
            ..ConfigOverrides::default()
        })
        .build()
        .await?;
    let server =
        AppServerClient::InProcess(crate::tests::start_test_embedded_app_server(config).await?);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: "--m".into(),
        mode: CompletionMode::Field,
    };
    spawn_completion(
        server.request_handle(),
        WorkflowCompletionTask {
            id: Some("team/complete".into()),
            cwd: project.path().to_path_buf(),
            generation: 7,
            workflow_dir: workflow_dir.clone(),
            request: request.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            tx: AppEventSender::new(tx),
        },
    );
    let event = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), rx.recv())
        .await?
        .ok_or_else(|| color_eyre::eyre::eyre!("completion channel closed"))?;
    let AppEvent::WorkflowCompletionResult {
        generation,
        workflow_dir: result_dir,
        request: result_request,
        result,
    } = event
    else {
        panic!("expected workflow completion: {event:?}");
    };
    assert_eq!(generation, 7);
    assert_eq!(result_dir, workflow_dir);
    assert_eq!(result_request, request);
    assert_eq!(
        result,
        CompletionResult::new(
            vec![CompletionItem {
                value: "--message".into(),
                description: Some("Message to include in the result.".into()),
            }],
            /*error*/ None,
        )
    );
    Ok(())
}
