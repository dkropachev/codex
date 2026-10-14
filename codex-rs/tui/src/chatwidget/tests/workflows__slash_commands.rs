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

fn sync_remote_workflow_commands(chat: &mut ChatWidget) {
    chat.sync_workflow_commands();
    let commands = codex_workflows::discover_workflow_commands(
        &chat.config.codex_home,
        &chat.workflow_invocation_cwd(),
    );
    chat.apply_workflow_commands_result(
        chat.workflow_commands_generation,
        chat.workflow_invocation_cwd(),
        Ok(commands),
    );
}

#[tokio::test]
async fn late_workflow_discovery_does_not_replace_newer_server_results() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let cwd = chat.workflow_invocation_cwd();
    let stale_generation = chat.workflow_commands_generation;
    chat.sync_workflow_commands();
    let current_generation = chat.workflow_commands_generation;
    let current_command = codex_workflows::WorkflowCommand {
        id: "team/review".to_string(),
        command: "review".to_string(),
        description: "Run review".to_string(),
        option_hints: Vec::new(),
        workflow_dir: PathBuf::from("/app-server/workflows/team/review"),
    };

    chat.apply_workflow_commands_result(
        stale_generation,
        cwd.clone(),
        Ok(vec![current_command.clone()]),
    );
    assert_eq!(chat.workflow_commands, Vec::new());

    chat.apply_workflow_commands_result(current_generation, cwd, Ok(vec![current_command.clone()]));
    assert_eq!(chat.workflow_commands, vec![current_command]);
}

#[tokio::test]
async fn workflow_discovery_error_is_visible() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.apply_workflow_commands_result(
        chat.workflow_commands_generation,
        chat.workflow_invocation_cwd(),
        Err("source unavailable".to_string()),
    );

    let cells = drain_insert_history_normalized(&mut rx);
    insta::assert_snapshot!(
        cells
            .iter()
            .map(|cell| lines_to_single_string(cell).trim().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
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
    sync_remote_workflow_commands(&mut chat);

    chat.bottom_pane
        .set_composer_text("/code".to_string(), Vec::new(), Vec::new());

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    insta::assert_snapshot!(popup);
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
    sync_remote_workflow_commands(&mut chat);

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
async fn workflow_management_shows_managed_release_and_update_state() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.dispatch_command(SlashCommand::Workflow);
    let request_id = chat
        .workflow_management_request_id
        .expect("management request");
    insta::assert_snapshot!(
        "workflow_management_loading",
        render_bottom_popup(&chat, /*width*/ 80)
    );

    let installed = codex_app_server_protocol::WorkflowReleaseIdentity {
        tag: Some("v1.0.0".to_string()),
        version: Some("1.0.0".to_string()),
        commit: "a".repeat(40),
    };
    let available = codex_app_server_protocol::WorkflowReleaseIdentity {
        tag: Some("v1.1.0".to_string()),
        version: Some("1.1.0".to_string()),
        commit: "b".repeat(40),
    };
    let cwd = chat.workflow_invocation_cwd();
    chat.on_workflow_management_loaded(
        request_id,
        cwd,
        Ok(crate::workflow_remote::WorkflowManagementData {
            managed: Vec::new(),
            workflows: vec![codex_app_server_protocol::WorkflowSummary {
                id: "team/review".to_string(),
                command: "team-review".to_string(),
                description: "Review changes".to_string(),
                path: "/app-server/workflows/team/review".to_string(),
                managed: Some(codex_app_server_protocol::ManagedWorkflowInfo {
                    source: "file:///app-server/sources/review".to_string(),
                    installed,
                    policy: codex_app_server_protocol::WorkflowUpdatePolicy::Prompt,
                    dismissed_release: None,
                }),
            }],
            updates: vec![codex_app_server_protocol::WorkflowUpdateEntry {
                id: "team/review".to_string(),
                status: codex_app_server_protocol::WorkflowUpdateStatus::Available,
                release: Some(available),
                dismissed: false,
                error: None,
            }],
            generation: Some(3),
            scanning: false,
            error: None,
        }),
    );
    insta::assert_snapshot!(
        "workflow_management_loaded",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let action = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::WorkflowManagementAction { action, .. } => Some(action),
        _ => None,
    });
    assert_matches!(
        action,
        Some(crate::workflow_actions::WorkflowAction::Update { id, .. }) if id == "team/review"
    );
}

#[tokio::test]
async fn workflow_management_shows_source_errors() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.dispatch_command(SlashCommand::Workflow);
    let request_id = chat
        .workflow_management_request_id
        .expect("management request");
    let cwd = chat.workflow_invocation_cwd();
    chat.on_workflow_management_loaded(
        request_id,
        cwd,
        Ok(crate::workflow_remote::WorkflowManagementData {
            managed: Vec::new(),
            workflows: vec![codex_app_server_protocol::WorkflowSummary {
                id: "team/review".to_string(),
                command: "team-review".to_string(),
                description: "Review changes".to_string(),
                path: "/app-server/workflows/team/review".to_string(),
                managed: Some(codex_app_server_protocol::ManagedWorkflowInfo {
                    source: "file:///app-server/sources/review".to_string(),
                    installed: codex_app_server_protocol::WorkflowReleaseIdentity {
                        tag: Some("v1.0.0".to_string()),
                        version: Some("1.0.0".to_string()),
                        commit: "a".repeat(40),
                    },
                    policy: codex_app_server_protocol::WorkflowUpdatePolicy::Manual,
                    dismissed_release: None,
                }),
            }],
            updates: vec![codex_app_server_protocol::WorkflowUpdateEntry {
                id: "team/review".to_string(),
                status: codex_app_server_protocol::WorkflowUpdateStatus::Error,
                release: None,
                dismissed: false,
                error: Some("repository moved".to_string()),
            }],
            generation: Some(4),
            scanning: false,
            error: Some("another source is unavailable".to_string()),
        }),
    );

    insta::assert_snapshot!(render_bottom_popup(&chat, /*width*/ 80));
}

#[tokio::test]
async fn workflow_management_request_failure_is_visible() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.dispatch_command(SlashCommand::Workflow);
    let request_id = chat
        .workflow_management_request_id
        .expect("management request");
    let cwd = chat.workflow_invocation_cwd();
    chat.on_workflow_management_loaded(request_id, cwd, Err("connection lost".to_string()));

    let cells = drain_insert_history_normalized(&mut rx);
    insta::assert_snapshot!(
        cells
            .iter()
            .map(|cell| lines_to_single_string(cell).trim().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[tokio::test]
async fn workflow_management_discards_response_after_cwd_refresh() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.dispatch_command(SlashCommand::Workflow);
    let request_id = chat
        .workflow_management_request_id
        .expect("management request");
    let cwd = chat.workflow_invocation_cwd();
    chat.sync_workflow_commands();
    chat.on_workflow_management_loaded(
        request_id,
        cwd,
        Ok(crate::workflow_remote::WorkflowManagementData {
            managed: Vec::new(),
            workflows: Vec::new(),
            updates: Vec::new(),
            generation: Some(1),
            scanning: false,
            error: None,
        }),
    );

    assert_eq!(chat.workflow_management_request_id, None);
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("No workflows found"));
}

#[tokio::test]
async fn prompt_updates_share_one_decision_view_with_per_release_choices() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let installed = codex_app_server_protocol::WorkflowReleaseIdentity {
        tag: Some("v1.0.0".into()),
        version: Some("1.0.0".into()),
        commit: "a".repeat(40),
    };
    let available = codex_app_server_protocol::WorkflowReleaseIdentity {
        tag: Some("v1.1.0".into()),
        version: Some("1.1.0".into()),
        commit: "b".repeat(40),
    };
    let data = crate::workflow_remote::WorkflowManagementData {
        workflows: Vec::new(),
        managed: ["team/review", "team/build"]
            .into_iter()
            .map(|id| codex_app_server_protocol::ManagedWorkflowEntry {
                id: id.to_string(),
                managed: codex_app_server_protocol::ManagedWorkflowInfo {
                    source: format!("file:///sources/{id}"),
                    installed: installed.clone(),
                    policy: codex_app_server_protocol::WorkflowUpdatePolicy::Prompt,
                    dismissed_release: None,
                },
            })
            .collect(),
        updates: ["team/review", "team/build"]
            .into_iter()
            .map(|id| codex_app_server_protocol::WorkflowUpdateEntry {
                id: id.to_string(),
                status: codex_app_server_protocol::WorkflowUpdateStatus::Available,
                release: Some(available.clone()),
                dismissed: false,
                error: None,
            })
            .collect(),
        generation: Some(7),
        scanning: false,
        error: None,
    };
    chat.show_workflow_update_decisions(data.clone());
    insta::assert_snapshot!(
        "workflow_update_decisions",
        render_bottom_popup(&chat, /*width*/ 80)
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let choice = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::OpenWorkflowUpdateDecision {
            id,
            installed,
            available,
            cwd,
        } => Some((id, installed, available, cwd)),
        _ => None,
    });
    let (id, selected_installed, selected_available, cwd) = choice.expect("choice event");
    chat.open_workflow_update_decision(id.clone(), selected_installed, selected_available, cwd);
    insta::assert_snapshot!(
        "workflow_update_choice",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let later = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::WorkflowUpdateLater { id, release } => Some((id, release)),
        _ => None,
    });
    let (later_id, later_release) = later.expect("later event");
    assert_eq!(later_id, id);
    chat.later_workflow_update(later_id, later_release);
    chat.show_workflow_update_decisions(data);
    insta::assert_snapshot!(
        "workflow_update_after_later",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn workflow_action_results_are_visible_and_success_refreshes() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let cwd = chat.workflow_invocation_cwd();
    chat.on_workflow_action_finished(cwd.clone(), Err("stale release".to_string()));
    chat.on_workflow_action_finished(cwd, Ok("Workflow policy updated".to_string()));

    let cells = drain_insert_history_normalized(&mut rx);
    insta::assert_snapshot!(
        cells
            .iter()
            .map(|cell| lines_to_single_string(cell).trim().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(chat.workflow_management_request_id.is_some());
}

#[tokio::test]
async fn workflow_uninstall_requires_explicit_confirmation() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let installed = codex_app_server_protocol::WorkflowReleaseIdentity {
        tag: Some("v1.0.0".to_string()),
        version: Some("1.0.0".to_string()),
        commit: "a".repeat(40),
    };
    chat.confirm_workflow_uninstall(
        "team/review".to_string(),
        installed.clone(),
        chat.workflow_invocation_cwd(),
    );
    insta::assert_snapshot!(render_bottom_popup(&chat, /*width*/ 80));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::WorkflowManagementAction { .. }))
    );

    chat.confirm_workflow_uninstall(
        "team/review".to_string(),
        installed.clone(),
        chat.workflow_invocation_cwd(),
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let action = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::WorkflowManagementAction { action, .. } => Some(action),
        _ => None,
    });
    assert_matches!(
        action,
        Some(crate::workflow_actions::WorkflowAction::Uninstall {
            id,
            installed: selected,
        }) if id == "team/review" && selected == installed
    );
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
async fn workflow_slash_with_args_dispatches_management_rpc() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);

    chat.dispatch_command_with_args(SlashCommand::Workflow, "list".to_string(), Vec::new());

    assert!(chat.workflow_management_request_id.is_some());
    assert_no_shell_or_user_turn(&mut op_rx);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn workflow_slash_update_uses_rpc_and_legacy_flags_use_host_cli() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    chat.dispatch_command_with_args(
        SlashCommand::Workflow,
        "update team/review".to_string(),
        Vec::new(),
    );
    let request = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::WorkflowSlashCommand { args, .. } => Some(args),
        _ => None,
    });
    assert_eq!(request, Some("update team/review".to_string()));

    chat.dispatch_command_with_args(
        SlashCommand::Workflow,
        "update --all".to_string(),
        Vec::new(),
    );
    let command = std::iter::from_fn(|| op_rx.try_recv().ok()).find_map(|op| match op {
        Op::RunUserShellCommand { command } => Some(command),
        _ => None,
    });
    assert_eq!(command, Some("codex workflow update --all".to_string()));

    chat.dispatch_command_with_args(
        SlashCommand::Workflow,
        "install ~/sources/review".to_string(),
        Vec::new(),
    );
    let command = std::iter::from_fn(|| op_rx.try_recv().ok()).find_map(|op| match op {
        Op::RunUserShellCommand { command } => Some(command),
        _ => None,
    });
    assert_eq!(
        command,
        Some("codex workflow install ~/sources/review".to_string())
    );
}

#[tokio::test]
async fn bare_workflow_command_dispatches_structured_workflow_op() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);

    submit_composer_text(&mut chat, "/code-review");

    let (submitted_workflow_dir, input) = next_workflow_command(&mut op_rx);
    assert_eq!(submitted_workflow_dir, workflow_dir);
    assert_eq!(input, json!({}));
    assert_eq!(recall_latest_after_clearing(&mut chat), "/code-review");
}

#[tokio::test]
async fn running_workflow_command_uses_standard_task_status_snapshot() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);

    submit_composer_text(&mut chat, "/code-review");
    let _ = next_workflow_command(&mut op_rx);
    handle_turn_started(&mut chat, "workflow-turn");

    let height = chat.desired_height(/*width*/ 80);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height))
        .expect("create terminal");
    terminal
        .draw(|frame| chat.render(frame.area(), frame.buffer_mut()))
        .expect("draw running workflow");
    assert_chatwidget_snapshot!(
        "running_workflow_command_uses_standard_task_status",
        normalized_backend_snapshot(terminal.backend()),
    );

    handle_turn_completed(&mut chat, "workflow-turn", /*duration_ms*/ None);

    assert!(!chat.bottom_pane.status_indicator_visible());
}

#[tokio::test]
async fn workflow_custom_output_limit_truncation_notice_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    complete_assistant_message(
        &mut chat,
        "workflow-message",
        "# Workflow report\n\nPartial output.\n\n[Workflow output truncated to 128 bytes.]",
        Some(MessagePhase::FinalAnswer),
    );

    let cells = drain_insert_history(&mut rx);
    assert_eq!(cells.len(), 1);
    assert_chatwidget_snapshot!(
        "workflow_custom_output_limit_truncation_notice",
        lines_to_single_string(&cells[0]),
    );
}

#[tokio::test]
async fn workflow_command_with_args_dispatches_structured_input_json() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);

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
        })
    );
    assert_eq!(
        recall_latest_after_clearing(&mut chat),
        "/code-review --action list-reports --output md --include-skipped-by-limit --allowed-areas tui --allowed-areas core"
    );
}

#[tokio::test]
async fn workflow_command_preserves_explicit_executor_working_directory() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    let workflow_dir = write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);

    submit_composer_text(
        &mut chat,
        r#"/code-review --input '{"workingDirectory":"C:\\remote\\project"}'"#,
    );

    assert_eq!(
        next_workflow_command(&mut op_rx),
        (
            workflow_dir,
            json!({ "workingDirectory": r"C:\remote\project" }),
        )
    );
}

#[tokio::test]
async fn workflow_command_rejects_malformed_args_without_clearing_draft() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);

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
    sync_remote_workflow_commands(&mut chat);
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
        })
    );
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn queued_malformed_workflow_command_reports_error_and_drains_next_input() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Workflows, /*enabled*/ true);
    write_workflow_command(&mut chat, "code-review", "Run review.");
    sync_remote_workflow_commands(&mut chat);
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
        rendered.contains("Invalid workflow arguments: invalid kebab-case flag name '--bad-'"),
        "expected invalid-args message, got:\n{rendered}"
    );
    assert!(chat.input_queue.queued_user_messages.is_empty());
}
