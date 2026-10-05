use pretty_assertions::assert_eq;

use super::*;

async fn configured_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, events, ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    (chat, events, ops)
}

#[tokio::test]
async fn default_handoff_submits_planning_prompt_in_handoff_plan_mode() {
    let (mut chat, _events, mut ops) = configured_chat().await;

    chat.dispatch_command(SlashCommand::Handoff);

    let Op::UserTurn {
        items,
        collaboration_mode: Some(mode),
        ..
    } = next_submit_op(&mut ops)
    else {
        panic!("expected handoff planning turn");
    };
    assert_eq!(
        items,
        vec![UserInput::Text {
            text: crate::handoff::manual_planning_prompt(""),
            text_elements: Vec::new(),
        }]
    );
    assert_eq!(mode.mode, ModeKind::Plan);
    assert_eq!(
        chat.collaboration_mode_label(),
        Some(crate::handoff::HANDOFF_MODE_NAME)
    );
    assert_chatwidget_snapshot!(
        "handoff_planning_footer",
        render_bottom_popup(&chat, /*width*/ 80),
    );
}

#[tokio::test]
async fn handoff_appears_in_slash_discovery() {
    let (mut chat, _events, _ops) = configured_chat().await;
    chat.bottom_pane
        .set_composer_text("/han".to_string(), Vec::new(), Vec::new());
    assert_chatwidget_snapshot!(
        "handoff_slash_discovery",
        render_bottom_popup(&chat, /*width*/ 80),
    );
}

#[tokio::test]
async fn ask_and_literal_option_guidance_shape_only_the_planning_prompt() {
    for (args, expected_guidance) in [
        ("--ask focus on the tests", "focus on the tests"),
        ("-- -x", "-x"),
    ] {
        let (mut chat, _events, mut ops) = configured_chat().await;
        chat.dispatch_command_with_args(SlashCommand::Handoff, args.to_string(), Vec::new());
        let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
            panic!("expected handoff planning turn");
        };
        assert_eq!(
            items,
            vec![UserInput::Text {
                text: crate::handoff::manual_planning_prompt(expected_guidance),
                text_elements: Vec::new(),
            }],
            "unexpected planning prompt for {args:?}"
        );
    }
}

#[tokio::test]
async fn leading_hyphen_guidance_is_rejected_before_submission() {
    let (mut chat, mut events, mut ops) = configured_chat().await;
    chat.dispatch_command_with_args(SlashCommand::Handoff, "--ask -x".to_string(), Vec::new());

    assert_no_submit_op(&mut ops);
    let history = drain_insert_history(&mut events)
        .into_iter()
        .map(|lines| lines_to_single_string(&lines))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("handoff_invalid_option_usage", history);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn bare_live_handoff_keeps_remote_only_attachment() {
    let (mut chat, _events, mut ops) = configured_chat().await;
    let url = "https://example.com/handoff.png".to_string();
    chat.set_remote_image_urls(vec![url.clone()]);
    chat.bottom_pane
        .set_composer_text("/handoff".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected handoff planning turn with remote image");
    };
    assert!(items.contains(&UserInput::Image {
        image: codex_app_server_protocol::ImageReference::Inline { url },
        detail: None,
    }));
    assert!(items.contains(&UserInput::Text {
        text: crate::handoff::manual_planning_prompt(""),
        text_elements: Vec::new(),
    }));
    assert!(chat.remote_image_urls().is_empty());
}

#[tokio::test]
async fn bare_queued_handoff_keeps_remote_only_attachment() {
    let (mut chat, _events, mut ops) = configured_chat().await;
    let url = "https://example.com/queued-handoff.png".to_string();
    let mut message = UserMessage::from("/handoff");
    message.remote_image_urls = vec![url.clone()];

    assert_eq!(
        chat.submit_queued_slash_prompt(QueuedUserMessage::new(
            message,
            QueuedInputAction::ParseSlash,
        )),
        QueueDrain::Stop,
    );
    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected queued handoff planning turn");
    };
    assert!(items.contains(&UserInput::Image {
        image: codex_app_server_protocol::ImageReference::Inline { url },
        detail: None,
    }));
}

#[tokio::test]
async fn handoff_guidance_keeps_local_image_and_text_element() {
    let (mut chat, _events, mut ops) = configured_chat().await;
    let placeholder = "[Image #1]";
    let command_prefix = "/handoff ";
    let command = format!("{command_prefix}{placeholder} inspect this");
    let image = PathBuf::from("/tmp/handoff-reference.png");
    chat.bottom_pane.set_composer_text(
        command,
        vec![TextElement::new(
            (command_prefix.len()..command_prefix.len() + placeholder.len()).into(),
            Some(placeholder.to_string()),
        )],
        vec![image.clone()],
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected planning turn with a local image");
    };
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
        panic!("expected handoff planning prompt after the image");
    };
    assert_eq!(
        text,
        &crate::handoff::manual_planning_prompt("[Image #1] inspect this"),
    );
    assert_eq!(text_elements.len(), 1);
}

#[tokio::test]
async fn remote_workspace_image_preparation_keeps_handoff_ownership() {
    let (mut chat, mut events, mut ops) = configured_chat().await;
    chat.snapshot_local_images = true;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handoff.png");
    image::RgbImage::new(/*width*/ 2, /*height*/ 2)
        .save(&path)
        .unwrap();
    chat.bottom_pane
        .set_composer_text("/handoff".to_string(), Vec::new(), vec![path]);

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(chat.handoff_mode_active());
    assert_no_submit_op(&mut ops);
    let image_id = loop {
        if let AppEvent::ImagesPrepared(id) = events.recv().await.unwrap() {
            break id;
        }
    };
    chat.on_images_prepared(image_id);

    let Op::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        panic!("expected image-backed handoff planning turn");
    };
    assert!(matches!(items.first(), Some(UserInput::Image { .. })));
    assert!(chat.handoff_mode_active());
    assert_no_submit_op(&mut ops);
}

#[tokio::test]
async fn protected_source_states_reject_handoff_and_restore_the_command() {
    for state in ["owned", "ephemeral", "queued", "pending", "modal", "goal"] {
        let (mut chat, _events, mut ops) = configured_chat().await;
        let image_url = format!("https://example.com/{state}.png");
        chat.set_remote_image_urls(vec![image_url.clone()]);
        match state {
            "owned" => chat.set_parent_owned_thread(),
            "ephemeral" => chat.config.ephemeral = true,
            "queued" => chat
                .input_queue
                .queued_user_messages
                .push_back(UserMessage::from("queued work").into()),
            "pending" => chat.input_queue.user_turn_pending_start = true,
            "modal" => chat
                .bottom_pane
                .show_selection_view(SelectionViewParams::picker()),
            "goal" => {
                let thread_id = chat.thread_id.expect("source thread");
                chat.current_goal_status = Some(GoalStatusState::new(
                    codex_app_server_protocol::ThreadGoal {
                        thread_id: thread_id.to_string(),
                        objective: "Finish existing work".to_string(),
                        status: codex_app_server_protocol::ThreadGoalStatus::Active,
                        token_budget: None,
                        tokens_used: 0,
                        time_used_seconds: 0,
                        created_at: 0,
                        updated_at: 0,
                    },
                    std::time::Instant::now(),
                ));
            }
            _ => unreachable!(),
        }

        chat.dispatch_command_with_args(
            SlashCommand::Handoff,
            "inspect this".to_string(),
            Vec::new(),
        );

        assert_no_submit_op(&mut ops);
        assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
        assert_eq!(
            chat.composer_text_with_pending(),
            "/handoff inspect this",
            "{state}"
        );
        assert_eq!(chat.remote_image_urls(), vec![image_url], "{state}");
    }
}

#[tokio::test]
async fn unsupported_handoff_image_restores_the_original_command() {
    let (mut chat, _events, mut ops) = configured_chat().await;
    let current_model = chat.current_model().to_string();
    let mut models = chat.model_catalog.try_list_models().expect("model catalog");
    models
        .iter_mut()
        .find(|model| model.model == current_model)
        .expect("current model")
        .input_modalities
        .retain(|modality| *modality != InputModality::Image);
    Arc::make_mut(&mut chat.model_catalog).models = models;
    let url = "https://example.com/unsupported.png".to_string();
    chat.set_remote_image_urls(vec![url.clone()]);
    chat.bottom_pane
        .set_composer_text("/handoff".to_string(), Vec::new(), Vec::new());

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_no_submit_op(&mut ops);
    assert_eq!(chat.bottom_pane.composer_text(), "/handoff");
    assert_eq!(chat.remote_image_urls(), vec![url]);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}
