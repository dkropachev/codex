use std::path::Path;
use std::path::PathBuf;

use codex_workflows::CompletionItem;
use codex_workflows::CompletionMode;
use codex_workflows::CompletionRequest;
use codex_workflows::CompletionResult;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc::unbounded_channel;

use super::AppEvent;
use super::ChatComposer;
use super::WorkflowCommand;
use super::workflow_completion_hints;
use super::workflow_completion_request;

fn command() -> WorkflowCommand {
    WorkflowCommand {
        id: "review".to_string(),
        command: "review".to_string(),
        description: "Review changes.".to_string(),
        option_hints: Vec::new(),
        workflow_dir: PathBuf::from("/workflows/review"),
    }
}

fn test_composer() -> (ChatComposer, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel();
    (
        ChatComposer::new(
            /*has_input_focus*/ true,
            crate::bottom_pane::AppEventSender::new(tx),
            /*enhanced_keys_supported*/ false,
            "Ask Codex to do anything".to_string(),
            /*disable_paste_burst*/ false,
        ),
        rx,
    )
}

#[test]
fn completion_request_tracks_partial_input_and_active_value() {
    let (workflow_dir, request) = workflow_completion_request(
        "/review --target-ref main --action re",
        "/review --target-ref main --action re".len(),
        &[command()],
        Path::new("/repo"),
    )
    .expect("completion request");

    assert_eq!(workflow_dir, PathBuf::from("/workflows/review"));
    assert_eq!(request.mode, CompletionMode::Value);
    assert_eq!(request.active_field.as_deref(), Some("action"));
    assert_eq!(request.prefix, "re");
    assert_eq!(
        request.input,
        json!({
            "targetRef": "main",
            "workingDirectory": "/repo",
        })
    );
}

#[test]
fn field_and_value_results_become_popup_hints() {
    let (_, field_request) = workflow_completion_request(
        "/review --tar",
        "/review --tar".len(),
        &[command()],
        Path::new("/repo"),
    )
    .expect("field request");
    assert_eq!(field_request.mode, CompletionMode::Field);
    assert_eq!(field_request.prefix, "--tar");
    assert_eq!(
        workflow_completion_hints(
            &field_request,
            &CompletionResult::new(
                vec![CompletionItem {
                    value: "--target-ref".to_string(),
                    description: Some("Target revision.".to_string()),
                }],
                /*error*/ None
            ),
        )[0]
        .display,
        "--target-ref"
    );

    let (_, value_request) = workflow_completion_request(
        "/review --action r",
        "/review --action r".len(),
        &[command()],
        Path::new("/repo"),
    )
    .expect("value request");
    assert_eq!(
        workflow_completion_hints(
            &value_request,
            &CompletionResult::new(
                vec![CompletionItem {
                    value: "review".to_string(),
                    description: None,
                }],
                /*error*/ None
            ),
        )[0]
        .display,
        "--action review"
    );
}

#[test]
fn value_completion_keeps_raw_filter_value_and_quotes_insertion() {
    let (_, request) = workflow_completion_request(
        "/review --action N",
        "/review --action N".len(),
        &[command()],
        Path::new("/repo"),
    )
    .expect("value request");
    let hints = workflow_completion_hints(
        &request,
        &CompletionResult::new(
            vec![CompletionItem {
                value: "New York".to_string(),
                description: None,
            }],
            /*error*/ None,
        ),
    );

    assert_eq!(hints[0].display, "--action 'New York'");
}

#[test]
fn stale_results_are_ignored_and_error_fallback_items_are_retained() {
    let (mut composer, _rx) = test_composer();
    let command = command();
    let workflow_dir = command.workflow_dir.clone();
    composer.workflow_commands = vec![command];
    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: "--".to_string(),
        mode: CompletionMode::Field,
    };
    composer.workflow_completion_generation = 2;
    composer.workflow_completion_request = Some((workflow_dir.clone(), request.clone()));

    composer.on_workflow_completion_result(
        /*generation*/ 1,
        workflow_dir.clone(),
        request.clone(),
        CompletionResult::new(
            vec![CompletionItem {
                value: "--stale".to_string(),
                description: None,
            }],
            /*error*/ None,
        ),
    );
    assert!(composer.workflow_commands[0].option_hints.is_empty());

    composer.on_workflow_completion_result(
        /*generation*/ 2,
        workflow_dir,
        request,
        CompletionResult::new(
            vec![CompletionItem {
                value: "--static".to_string(),
                description: Some("Schema field.".to_string()),
            }],
            Some("dynamic hook timed out".to_string()),
        ),
    );
    assert_eq!(
        composer.workflow_commands[0].option_hints[0].display,
        "--static"
    );
}

#[test]
fn leaving_workflow_context_emits_completion_cancellation() {
    let (mut composer, mut rx) = test_composer();
    let request = CompletionRequest {
        input: json!({}),
        active_field: None,
        prefix: String::new(),
        mode: CompletionMode::Field,
    };
    composer.workflow_completion_request = Some((PathBuf::from("/workflow"), request));

    composer.sync_workflow_completion("plain text", "plain text".len());

    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::CancelWorkflowCompletion)
    ));
}
