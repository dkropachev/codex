use super::*;
use pretty_assertions::assert_eq;

async fn configured_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    (chat, rx, op_rx)
}

fn submitted_text_and_mode(
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
) -> (String, CollaborationMode) {
    match next_submit_op(op_rx) {
        Op::UserTurn {
            items,
            collaboration_mode: Some(collaboration_mode),
            ..
        } => {
            let [UserInput::Text { text, .. }] = items.as_slice() else {
                panic!("expected one text item, got {items:?}");
            };
            (text.clone(), collaboration_mode)
        }
        other => panic!("expected handoff UserTurn, got {other:?}"),
    }
}

fn history_text(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> String {
    drain_insert_history(rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn plan_handoff_default_command_submits_planning_prompt_in_displayed_handoff_plan_mode() {
    let (mut chat, _rx, mut op_rx) = configured_chat().await;

    chat.dispatch_command(SlashCommand::Handoff);

    let (text, collaboration_mode) = submitted_text_and_mode(&mut op_rx);
    assert_eq!(text, crate::handoff::manual_planning_prompt(""));
    assert_eq!(collaboration_mode.mode, ModeKind::Plan);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);
    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
}

#[tokio::test]
async fn plan_handoff_inline_options_and_double_dash_shape_only_the_planning_prompt() {
    for (args, expected_guidance) in [
        ("focus the parser", "focus the parser"),
        ("--ask focus the tests", "focus the tests"),
        ("--defer document blockers", "document blockers"),
        (
            "-- --defer is literal guidance",
            "--defer is literal guidance",
        ),
    ] {
        let (mut chat, _rx, mut op_rx) = configured_chat().await;

        chat.dispatch_command_with_args(SlashCommand::Handoff, args.to_string(), Vec::new());

        let (text, collaboration_mode) = submitted_text_and_mode(&mut op_rx);
        assert_eq!(
            text,
            crate::handoff::manual_planning_prompt(expected_guidance),
            "unexpected prompt for {args:?}"
        );
        assert_eq!(collaboration_mode.mode, ModeKind::Plan);
    }
}

#[tokio::test]
async fn plan_handoff_unknown_and_conflicting_options_show_usage_without_submission() {
    for (args, expected) in [
        ("--later guidance", "Unknown /handoff option `--later`."),
        (
            "--ask --defer guidance",
            "`--ask` and `--defer` cannot be used together.",
        ),
    ] {
        let (mut chat, mut rx, mut op_rx) = configured_chat().await;

        chat.dispatch_command_with_args(SlashCommand::Handoff, args.to_string(), Vec::new());

        assert_no_submit_op(&mut op_rx);
        let rendered = history_text(&mut rx);
        assert!(
            rendered.contains(expected),
            "unexpected error: {rendered:?}"
        );
        assert!(rendered.contains(crate::handoff::HANDOFF_USAGE));
        assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    }
}

#[tokio::test]
async fn plan_handoff_leading_bang_guidance_is_sent_literally_and_never_run_as_shell() {
    let (mut chat, _rx, mut op_rx) = configured_chat().await;

    chat.dispatch_command_with_args(
        SlashCommand::Handoff,
        "!echo should-not-run".to_string(),
        Vec::new(),
    );

    let (text, collaboration_mode) = submitted_text_and_mode(&mut op_rx);
    assert_eq!(
        text,
        crate::handoff::manual_planning_prompt("!echo should-not-run")
    );
    assert_eq!(collaboration_mode.mode, ModeKind::Plan);
    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn plan_handoff_composer_submission_preserves_guidance_attachment_and_text_element() {
    let (mut chat, _rx, mut op_rx) = configured_chat().await;
    let placeholder = "[Image #1]";
    let command_prefix = "/handoff ";
    let composer_text = format!("{command_prefix}{placeholder} inspect this");
    let placeholder_start = command_prefix.len();
    let text_elements = vec![TextElement::new(
        (placeholder_start..placeholder_start + placeholder.len()).into(),
        Some(placeholder.to_string()),
    )];
    let image = PathBuf::from("/tmp/handoff-reference.png");
    chat.bottom_pane
        .set_composer_text(composer_text, text_elements, vec![image.clone()]);

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match next_submit_op(&mut op_rx) {
        Op::UserTurn {
            items,
            collaboration_mode: Some(collaboration_mode),
            ..
        } => {
            assert_eq!(
                items.first(),
                Some(&UserInput::LocalImage {
                    path: image,
                    detail: None,
                })
            );
            let Some(UserInput::Text {
                text,
                text_elements,
            }) = items.get(1)
            else {
                panic!("expected handoff prompt after attachment, got {items:?}");
            };
            assert_eq!(
                text,
                &crate::handoff::manual_planning_prompt("[Image #1] inspect this")
            );
            assert_eq!(text_elements.len(), 1);
            assert_eq!(collaboration_mode.mode, ModeKind::Plan);
        }
        other => panic!("expected handoff UserTurn with attachment, got {other:?}"),
    }
}

#[tokio::test]
async fn plan_handoff_command_is_rejected_outside_default_mode_without_changing_plan_behavior() {
    let (mut chat, mut rx, mut op_rx) = configured_chat().await;
    let plan_mask = collaboration_modes::plan_mask(chat.model_catalog.as_ref())
        .expect("plan collaboration mode");
    chat.set_collaboration_mask(plan_mask);
    let _ = drain_insert_history(&mut rx);

    chat.dispatch_command(SlashCommand::Handoff);

    assert_no_submit_op(&mut op_rx);
    assert!(
        history_text(&mut rx)
            .contains("/handoff is available only from an idle Default-mode session.")
    );
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Plan);

    let (mut chat, _rx, mut op_rx) = configured_chat().await;
    chat.dispatch_command_with_args(
        SlashCommand::Plan,
        "keep existing plan behavior".to_string(),
        Vec::new(),
    );
    match next_submit_op(&mut op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "keep existing plan behavior".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected unchanged /plan UserTurn, got {other:?}"),
    }
}
