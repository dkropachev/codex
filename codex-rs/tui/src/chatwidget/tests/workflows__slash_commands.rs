use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;

fn write_workflow_command(chat: &mut ChatWidget, command: &str, description: &str) -> PathBuf {
    let workflow_dir = chat.config.codex_home.join("workflows").join(command);
    fs::create_dir_all(&workflow_dir).expect("create workflow dir");
    fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {command}\ncommand: {command}\ntitle: /{command}\nuserDescription: {description}\n"
        ),
    )
    .expect("write workflow yaml");
    workflow_dir.to_path_buf()
}

fn next_shell_command(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> String {
    loop {
        match op_rx.try_recv() {
            Ok(Op::RunUserShellCommand { command }) => return command,
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected shell command op but queue was empty"),
            Err(TryRecvError::Disconnected) => {
                panic!("expected shell command op but channel closed")
            }
        }
    }
}

fn next_workflow_command(
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
) -> (PathBuf, serde_json::Value) {
    loop {
        match op_rx.try_recv() {
            Ok(Op::RunWorkflowCommand {
                workflow_dir,
                input,
            }) => return (workflow_dir, input),
            Ok(Op::RunUserShellCommand { command }) => {
                panic!("unexpected shell command op for workflow command: {command:?}")
            }
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected workflow command op but queue was empty"),
            Err(TryRecvError::Disconnected) => {
                panic!("expected workflow command op but channel closed")
            }
        }
    }
}

fn assert_no_shell_or_user_turn(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) {
    while let Ok(op) = op_rx.try_recv() {
        assert!(
            !matches!(
                op,
                Op::RunUserShellCommand { .. }
                    | Op::RunWorkflowCommand { .. }
                    | Op::UserTurn { .. }
            ),
            "unexpected submit op: {op:?}"
        );
    }
}

fn next_user_turn_without_shell(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    loop {
        match op_rx.try_recv() {
            Ok(op @ Op::UserTurn { .. }) => return op,
            Ok(op @ Op::RunUserShellCommand { .. }) => {
                panic!("unexpected shell command op before user turn: {op:?}")
            }
            Ok(op @ Op::RunWorkflowCommand { .. }) => {
                panic!("unexpected workflow command op before user turn: {op:?}")
            }
            Ok(_) => continue,
            Err(TryRecvError::Empty) => panic!("expected user turn op but queue was empty"),
            Err(TryRecvError::Disconnected) => panic!("expected user turn op but channel closed"),
        }
    }
}

fn complete_turn_with_message(chat: &mut ChatWidget, turn_id: &str, message: Option<&str>) {
    if let Some(message) = message {
        complete_assistant_message(
            chat,
            &format!("{turn_id}-message"),
            message,
            Some(MessagePhase::FinalAnswer),
        );
    }
    handle_turn_completed(chat, turn_id, /*duration_ms*/ None);
}

fn submit_composer_text(chat: &mut ChatWidget, text: &str) {
    chat.bottom_pane
        .set_composer_text(text.to_string(), Vec::new(), Vec::new());
    submit_current_composer(chat);
}

fn submit_current_composer(chat: &mut ChatWidget) {
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
}

fn queue_composer_text_with_tab(chat: &mut ChatWidget, text: &str) {
    chat.bottom_pane
        .set_composer_text(text.to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
}

fn recall_latest_after_clearing(chat: &mut ChatWidget) -> String {
    chat.bottom_pane
        .set_composer_text(String::new(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    chat.bottom_pane.composer_text()
}

#[tokio::test]
async fn workflow_command_appears_in_slash_popup_when_enabled() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(
        &mut chat,
        "code-review",
        "Run a code review workflow on the current branch.",
    );
    chat.sync_workflow_commands();

    chat.bottom_pane
        .set_composer_text("/code".to_string(), Vec::new(), Vec::new());

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("/code-review"),
        "expected workflow command in popup:\n{popup}"
    );
    assert!(
        popup.contains("Run a code review workflow"),
        "expected workflow description in popup:\n{popup}"
    );
}

#[tokio::test]
async fn workflow_command_is_hidden_and_rejected_when_feature_disabled() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    write_workflow_command(&mut chat, "code-review", "Run a code review workflow.");
    chat.sync_workflow_commands();

    chat.bottom_pane
        .set_composer_text("/code".to_string(), Vec::new(), Vec::new());

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        !popup.contains("/code-review"),
        "expected workflow command to be hidden while disabled:\n{popup}"
    );

    chat.bottom_pane.set_composer_text(
        "/code-review --action list-reports".to_string(),
        Vec::new(),
        Vec::new(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_no_shell_or_user_turn(&mut op_rx);
    let cells = drain_insert_history(&mut rx);
    let rendered = cells
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Unrecognized command '/code-review'"),
        "expected unknown-command message, got:\n{rendered}"
    );
}

#[tokio::test]
async fn bare_workflow_slash_enters_workflow_mode() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let initial = chat.current_collaboration_mode().clone();
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.bottom_pane.set_task_running(/*running*/ false);

    chat.dispatch_command(SlashCommand::Workflow);

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Workflow);
    assert_eq!(chat.current_collaboration_mode(), &initial);
    assert_no_shell_or_user_turn(&mut op_rx);
}

#[tokio::test]
async fn bare_workflow_slash_reports_disabled_when_feature_off() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.dispatch_command(SlashCommand::Workflow);

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert_no_shell_or_user_turn(&mut op_rx);
    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Workflows are disabled."),
        "expected disabled workflow message, got:\n{rendered}"
    );
    assert!(
        rendered.contains("Enable [features].workflows to use /workflow."),
        "expected workflow feature hint, got:\n{rendered}"
    );
}

#[tokio::test]
async fn workflow_done_slash_exits_to_default_mode() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);

    chat.dispatch_command(SlashCommand::Workflow);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Workflow);
    let _ = drain_insert_history(&mut rx);

    chat.dispatch_command_with_args(SlashCommand::Workflow, "done".to_string(), Vec::new());

    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert_no_shell_or_user_turn(&mut op_rx);
    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Workflow mode disabled."),
        "expected workflow disabled message, got:\n{rendered}"
    );
}

#[tokio::test]
async fn workflow_slash_with_args_dispatches_workflow_cli_command() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);

    chat.dispatch_command_with_args(SlashCommand::Workflow, "list".to_string(), Vec::new());

    assert_eq!(next_shell_command(&mut op_rx), "codex workflow list");
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn bare_workflow_command_dispatches_structured_workflow_op() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    chat.sync_workflow_commands();

    submit_composer_text(&mut chat, "/code-review");

    let (submitted_workflow_dir, input) = next_workflow_command(&mut op_rx);
    assert_eq!(submitted_workflow_dir, workflow_dir);
    assert_eq!(
        input,
        json!({ "workingDirectory": test_path_display("/tmp/project") })
    );
    assert_eq!(recall_latest_after_clearing(&mut chat), "/code-review");
}

#[tokio::test]
async fn workflow_command_with_args_dispatches_structured_input_json() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    chat.sync_workflow_commands();

    submit_composer_text(
        &mut chat,
        "/code-review --action list-reports --output md --include-skipped-by-limit --allowed-areas tui --allowed-areas core",
    );

    let (submitted_workflow_dir, input) = next_workflow_command(&mut op_rx);
    assert_eq!(submitted_workflow_dir, workflow_dir);
    assert_eq!(
        input,
        json!({
            "action": "list-reports",
            "output": "md",
            "includeSkippedByLimit": true,
            "allowedAreas": ["tui", "core"],
            "workingDirectory": test_path_display("/tmp/project"),
        })
    );
    assert_eq!(
        recall_latest_after_clearing(&mut chat),
        "/code-review --action list-reports --output md --include-skipped-by-limit --allowed-areas tui --allowed-areas core"
    );
}

#[tokio::test]
async fn workflow_command_rejects_malformed_args_without_clearing_draft() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(&mut chat, "code-review", "Run review.");
    chat.sync_workflow_commands();

    let bad_command = "/code-review --input '{bad}'";
    chat.bottom_pane
        .set_composer_text(bad_command.to_string(), Vec::new(), Vec::new());
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_no_shell_or_user_turn(&mut op_rx);
    assert_eq!(chat.bottom_pane.composer_text(), bad_command);
    let cells = drain_insert_history(&mut rx);
    let rendered = cells
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Invalid workflow arguments: --input is not valid JSON"),
        "expected invalid-args message, got:\n{rendered}"
    );
}

#[tokio::test]
async fn queued_workflow_command_dispatches_after_active_turn() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    chat.sync_workflow_commands();
    chat.thread_id = Some(ThreadId::new());
    handle_turn_started(&mut chat, "turn-1");

    queue_composer_text_with_tab(&mut chat, "/code-review --action report");

    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));

    complete_turn_with_message(&mut chat, "turn-1", Some("done"));

    let (submitted_workflow_dir, input) = next_workflow_command(&mut op_rx);
    assert_eq!(submitted_workflow_dir, workflow_dir);
    assert_eq!(
        input,
        json!({
            "action": "report",
            "workingDirectory": test_path_display("/tmp/project"),
        })
    );
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn queued_malformed_workflow_command_reports_error_and_drains_next_input() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(&mut chat, "code-review", "Run review.");
    chat.sync_workflow_commands();
    chat.thread_id = Some(ThreadId::new());
    handle_turn_started(&mut chat, "turn-1");

    queue_composer_text_with_tab(&mut chat, "/code-review --bad-");
    queue_composer_text_with_tab(&mut chat, "hello after bad workflow");

    complete_turn_with_message(&mut chat, "turn-1", Some("done"));

    match next_user_turn_without_shell(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "hello after bad workflow".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued message after invalid workflow, got {other:?}"),
    }
    let cells = drain_insert_history(&mut rx);
    let rendered = cells
        .iter()
        .map(|cell| lines_to_single_string(cell))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Invalid workflow arguments: invalid flag name '--bad-'"),
        "expected invalid-args message, got:\n{rendered}"
    );
    assert!(chat.input_queue.queued_user_messages.is_empty());
}
