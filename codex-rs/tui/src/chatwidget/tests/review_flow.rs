use super::*;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

const REVIEW_TURN_ID: &str = "review-turn";

#[tokio::test]
async fn review_scope_loading_picker_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;

    chat.open_review_popup();

    assert_chatwidget_snapshot!(
        "review_scope_loading_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_scope_request_uses_current_thread_and_maps_response() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = ThreadId::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let expected = crate::review_scope::ReviewScopeResolution {
        default_branch: Some("main".to_string()),
        default_branch_target: Some("refs/remotes/origin/main".to_string()),
        branches: vec!["refs/remotes/origin/main".to_string()],
        ..Default::default()
    };
    chat.thread_id = Some(thread_id);
    chat.review_scope_resolver = Some(Arc::new(FakeReviewScopeResolver {
        calls: Arc::clone(&calls),
        result: Ok(expected.clone()),
    }));

    chat.open_review_popup();
    let (_, _, resolution) = next_scope_resolution(&mut rx).await;

    assert_eq!(resolution, expected);
    assert_eq!(*calls.lock().expect("calls lock"), vec![thread_id]);
}

#[tokio::test]
async fn review_scope_request_failure_falls_back_to_uncommitted() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.review_scope_resolver = Some(Arc::new(FakeReviewScopeResolver {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Err("environment unavailable".to_string()),
    }));

    chat.open_review_popup();
    let (request_id, cwd, resolution) = next_scope_resolution(&mut rx).await;

    assert_eq!(resolution, Default::default());
    assert!(chat.apply_review_scope_resolution(request_id, cwd, resolution));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(
        rx.recv().await.expect("action event"),
        AppEvent::OpenReviewActionPicker {
            thread_id: _,
            cwd: _,
            target: ReviewTarget::UncommittedChanges
        }
    );
}

#[tokio::test]
async fn review_scope_pull_request_picker_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            pull_request: Some(crate::review_scope::ReviewPullRequest {
                number: 314,
                url: "https://github.com/acme/widgets/pull/314".to_string(),
                base_branch: Some("main".to_string()),
                base_branch_target: Some("refs/remotes/origin/main".to_string()),
            }),
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec![
                "refs/remotes/origin/main".to_string(),
                "refs/heads/feature/review".to_string(),
            ],
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_pull_request_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewActionPicker {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("pull request scope selection")
    else {
        panic!("expected review action picker event");
    };
    assert_eq!(thread_id, chat.thread_id);
    assert_eq!(cwd, chat.config.cwd.to_path_buf());
    assert_eq!(
        target,
        ReviewTarget::PullRequest {
            url: "https://github.com/acme/widgets/pull/314".to_string(),
        }
    );
}

#[tokio::test]
async fn review_scope_default_branch_picker_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            pull_request: None,
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec![
                "refs/remotes/origin/main".to_string(),
                "refs/heads/feature/review".to_string(),
            ],
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_default_branch_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewActionPicker { target, .. } =
            rx.try_recv().expect("scope selection event")
        {
            break target;
        }
    };
    assert_eq!(
        target,
        ReviewTarget::BaseBranch {
            branch: "refs/remotes/origin/main".to_string(),
        }
    );
}

#[tokio::test]
async fn review_scope_falls_back_to_uncommitted_changes_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;

    assert_chatwidget_snapshot!(
        "review_scope_uncommitted_fallback_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewActionPicker { target, .. } =
            rx.try_recv().expect("scope selection event")
        {
            break target;
        }
    };
    assert_eq!(target, ReviewTarget::UncommittedChanges);
}

#[tokio::test]
async fn review_advanced_branch_picker_prefers_discovered_base_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            pull_request: Some(crate::review_scope::ReviewPullRequest {
                number: 314,
                url: "https://github.com/acme/widgets/pull/314".to_string(),
                base_branch: Some("release".to_string()),
                base_branch_target: Some("refs/heads/release".to_string()),
            }),
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec![
                "refs/heads/release".to_string(),
                "refs/heads/feature/review".to_string(),
                "refs/heads/main".to_string(),
            ],
        },
    )
    .await;
    chat.show_review_branch_picker(chat.thread_id, &cwd);

    assert_chatwidget_snapshot!(
        "review_advanced_branch_picker_preferred_base",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewActionPicker { target, .. } =
            rx.try_recv().expect("branch selection event")
        {
            break target;
        }
    };
    assert_eq!(
        target,
        ReviewTarget::BaseBranch {
            branch: "refs/remotes/origin/main".to_string(),
        }
    );
}

#[tokio::test]
async fn review_advanced_branch_picker_labels_remote_default_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec![
                "refs/remotes/origin/main".to_string(),
                "refs/heads/feature/review".to_string(),
            ],
            ..Default::default()
        },
    )
    .await;
    chat.show_review_branch_picker(chat.thread_id, &cwd);

    assert_chatwidget_snapshot!(
        "review_advanced_branch_picker_default_branch",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewActionPicker { target, .. } =
            rx.try_recv().expect("branch selection event")
        {
            break target;
        }
    };
    assert_eq!(
        target,
        ReviewTarget::BaseBranch {
            branch: "refs/remotes/origin/main".to_string(),
        }
    );
}

#[tokio::test]
async fn unresolved_pull_request_base_does_not_label_default_branch_as_pr_base() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            pull_request: Some(crate::review_scope::ReviewPullRequest {
                number: 314,
                url: "https://github.com/acme/widgets/pull/314".to_string(),
                base_branch: Some("main".to_string()),
                base_branch_target: None,
            }),
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec!["refs/remotes/origin/main".to_string()],
        },
    )
    .await;
    chat.show_review_branch_picker(chat.thread_id, &cwd);

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Pull request base"));
}

#[tokio::test]
async fn review_action_picker_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;

    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
    );

    assert_chatwidget_snapshot!(
        "review_action_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_action_picker_emits_fix_choices_with_thread_cwd_and_target() {
    let target = ReviewTarget::Custom {
        instructions: "check regressions".to_string(),
    };
    for (down_presses, expected_action) in [(1, ReviewAction::Fix), (2, ReviewAction::FixAndCommit)]
    {
        let (mut chat, mut rx, _op_rx) = review_chat().await;
        let thread_id = chat.thread_id;
        let cwd = chat.config.cwd.to_path_buf();
        chat.show_review_action_picker(thread_id, cwd.clone(), target.clone());
        for _ in 0..down_presses {
            chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            rx.try_recv(),
            Ok(AppEvent::StartReview {
                thread_id: event_thread_id,
                cwd: event_cwd,
                target: event_target,
                action,
            }) if event_thread_id == thread_id
                && event_cwd == cwd
                && event_target == target
                && action == expected_action
        );
    }
}

#[tokio::test]
async fn report_action_never_starts_a_follow_up_turn() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::Report);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 2);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn fix_action_starts_default_mode_turn_for_nonempty_findings() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let plan_mask =
        collaboration_modes::plan_mask(chat.model_catalog.as_ref()).expect("plan mode available");
    chat.set_collaboration_mask(plan_mask);
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 2);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    let (prompt, mode) = next_text_turn(&mut op_rx);
    assert!(prompt.contains("Revalidate every finding"));
    assert!(prompt.contains("report any findings you rejected"));
    assert!(!prompt.contains("create one focused new commit"));
    assert_eq!(mode, Some(ModeKind::Default));
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn fix_and_commit_action_requests_one_scoped_commit_without_push() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::FixAndCommit);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    let (prompt, mode) = next_text_turn(&mut op_rx);
    assert!(prompt.contains("create one focused new commit"));
    assert!(prompt.contains("Preserve unrelated working-tree changes"));
    assert!(prompt.contains("do not amend"));
    assert!(prompt.contains("do not push"));
    assert_eq!(mode, Some(ModeKind::Default));
}

#[tokio::test]
async fn fix_action_switches_from_config_mode_to_default() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let config_mask =
        crate::config_mode::config_plan_mask(&chat.config.cwd, &chat.config.codex_home);
    chat.set_collaboration_mask(config_mask);
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    let (_, mode) = next_text_turn(&mut op_rx);
    assert_eq!(mode, Some(ModeKind::Default));
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
}

#[tokio::test]
async fn clean_and_interrupted_reviews_do_not_start_follow_up_turns() {
    let (mut clean_chat, _rx, mut clean_ops) = review_chat().await;
    begin_review(&mut clean_chat, &mut clean_ops, ReviewAction::Fix);
    handle_exited_review_mode_with_findings(&mut clean_chat, /*finding_count*/ 0);
    handle_turn_completed(&mut clean_chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    assert_no_submit_op(&mut clean_ops);

    let (mut interrupted_chat, _rx, mut interrupted_ops) = review_chat().await;
    begin_review(
        &mut interrupted_chat,
        &mut interrupted_ops,
        ReviewAction::FixAndCommit,
    );
    handle_exited_review_mode_with_findings(&mut interrupted_chat, /*finding_count*/ 3);
    handle_turn_interrupted(&mut interrupted_chat, REVIEW_TURN_ID);
    assert_no_submit_op(&mut interrupted_ops);
}

#[tokio::test]
async fn failed_review_clears_fix_action_without_follow_up() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::FixAndCommit);
    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 3);

    chat.handle_server_notification(
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: chat.thread_id.expect("thread id").to_string(),
            turn: app_server_turn(
                REVIEW_TURN_ID,
                AppServerTurnStatus::Failed,
                /*duration_ms*/ None,
                /*error*/ None,
            ),
        }),
        /*replay_kind*/ None,
    );

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn opening_scope_picker_does_not_cancel_active_fix_action() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);
    chat.open_review_popup();

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    assert_no_submit_op(&mut op_rx);

    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let (prompt, mode) = next_text_turn(&mut op_rx);
    assert!(prompt.contains("Revalidate every finding"));
    assert_eq!(mode, Some(ModeKind::Default));
}

#[tokio::test]
async fn second_review_does_not_replace_active_fix_action() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);

    chat.start_review_for_thread(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
        ReviewAction::Report,
    );
    assert_no_submit_op(&mut op_rx);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    let (prompt, mode) = next_text_turn(&mut op_rx);
    assert!(prompt.contains("Revalidate every finding"));
    assert_eq!(mode, Some(ModeKind::Default));
}

#[tokio::test]
async fn replayed_or_stale_review_completion_clears_fix_action() {
    let (mut replay_chat, _rx, mut replay_ops) = review_chat().await;
    begin_review(&mut replay_chat, &mut replay_ops, ReviewAction::Fix);
    replay_entered_review_mode(&mut replay_chat, "replayed review");
    replay_chat.replay_thread_item(
        AppServerThreadItem::ExitedReviewMode {
            id: "replayed-review-end".to_string(),
            review: "finding".to_string(),
            finding_count: 1,
        },
        REVIEW_TURN_ID.to_string(),
        ReplayKind::ThreadSnapshot,
    );
    handle_turn_completed(&mut replay_chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    assert_no_submit_op(&mut replay_ops);

    let (mut stale_chat, _rx, mut stale_ops) = review_chat().await;
    begin_review(&mut stale_chat, &mut stale_ops, ReviewAction::Fix);
    complete_review_item_for_turn(&mut stale_chat, "stale-turn", /*finding_count*/ 2);
    handle_turn_completed(&mut stale_chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    assert_no_submit_op(&mut stale_ops);
}

#[tokio::test]
async fn review_action_is_cleared_when_thread_changes() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let original_thread_id = chat.thread_id.expect("thread id");
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);
    chat.handle_thread_session(configured_thread_session(ThreadId::new()));

    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: original_thread_id.to_string(),
            turn_id: REVIEW_TURN_ID.to_string(),
            completed_at_ms: 0,
            item: AppServerThreadItem::ExitedReviewMode {
                id: "review-end".to_string(),
                review: "finding".to_string(),
                finding_count: 1,
            },
        }),
        /*replay_kind*/ None,
    );
    chat.handle_server_notification(
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: original_thread_id.to_string(),
            turn: app_server_turn(
                REVIEW_TURN_ID,
                AppServerTurnStatus::Completed,
                /*duration_ms*/ None,
                /*error*/ None,
            ),
        }),
        /*replay_kind*/ None,
    );

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn stale_action_picker_selection_does_not_start_review_on_new_thread() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReview {
        thread_id,
        cwd,
        target,
        action,
    } = rx.try_recv().expect("review action event")
    else {
        panic!("expected review action event");
    };
    chat.thread_id = Some(ThreadId::new());

    chat.start_review_for_thread(thread_id, cwd, target, action);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn stale_action_picker_selection_does_not_start_review_after_cwd_change() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReview {
        thread_id,
        cwd,
        target,
        action,
    } = rx.try_recv().expect("review action event")
    else {
        panic!("expected review action event");
    };
    chat.config.cwd = test_path_buf("/tmp/other-review-cwd").abs();

    chat.start_review_for_thread(thread_id, cwd, target, action);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn scope_selection_does_not_open_action_picker_after_cwd_change() {
    let (mut chat, mut rx, _op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    chat.config.cwd = test_path_buf("/tmp/other-review-scope").abs();

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewActionPicker {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review action picker event")
    else {
        panic!("expected review action picker event");
    };
    chat.show_review_action_picker(thread_id, cwd, target);

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Choose a review action"));
}

#[tokio::test]
async fn scope_selection_does_not_open_action_picker_after_same_cwd_thread_change() {
    let (mut chat, mut rx, _op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    let next_thread = ThreadId::new();
    // Leave the picker intact to exercise its originating-thread guard directly; the normal
    // thread-session path may additionally dismiss or reset transient views.
    chat.thread_id = Some(next_thread);

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewActionPicker {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review action picker event")
    else {
        panic!("expected review action picker event");
    };
    chat.show_review_action_picker(thread_id, cwd, target);

    assert_eq!(chat.thread_id, Some(next_thread));
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Choose a review action"));
}

#[tokio::test]
async fn rejected_review_steer_runs_before_automatic_fix() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx, ReviewAction::Fix);
    chat.submit_user_message(UserMessage::from("user steer before fixes"));
    let (steer, _) = next_text_turn(&mut op_rx);
    assert_eq!(steer, "user steer before fixes");
    handle_error(
        &mut chat,
        "cannot steer a review turn",
        Some(CodexErrorInfo::ActiveTurnNotSteerable {
            turn_kind: NonSteerableTurnKind::Review,
        }),
    );

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);
    let (queued_steer, _) = next_text_turn(&mut op_rx);
    assert_eq!(queued_steer, "user steer before fixes");
    assert_no_submit_op(&mut op_rx);
    chat.bind_live_review_action(
        &chat.thread_id.expect("thread id").to_string(),
        REVIEW_TURN_ID.to_string(),
    );

    handle_turn_started(&mut chat, "steer-turn");
    handle_turn_completed(&mut chat, "steer-turn", /*duration_ms*/ None);
    let (fix_prompt, mode) = next_text_turn(&mut op_rx);
    assert!(fix_prompt.contains("Revalidate every finding"));
    assert_eq!(mode, Some(ModeKind::Default));
}

#[tokio::test]
async fn stale_scope_results_do_not_replace_current_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.open_review_popup();
    let (request_id, cwd, _) = next_scope_resolution(&mut rx).await;
    let stale_request_id = uuid::Uuid::new_v4();

    assert!(
        !chat.apply_review_scope_resolution(stale_request_id, cwd.clone(), Default::default(),)
    );
    assert!(
        !chat.apply_review_scope_resolution(request_id, cwd.join("other"), Default::default(),)
    );
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Loading review scopes"));

    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!chat.apply_review_scope_resolution(request_id, cwd, Default::default()));
    assert!(chat.is_normal_backtrack_mode());
}

async fn review_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, rx, op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    (chat, rx, op_rx)
}

fn begin_review(
    chat: &mut ChatWidget,
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
    action: ReviewAction,
) {
    chat.start_review_for_thread(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
        action,
    );
    assert_matches!(
        op_rx.try_recv(),
        Ok(Op::Review {
            target: ReviewTarget::UncommittedChanges
        })
    );
    chat.bind_live_review_action(
        &chat.thread_id.expect("thread id").to_string(),
        REVIEW_TURN_ID.to_string(),
    );
    handle_turn_started(chat, REVIEW_TURN_ID);
    handle_entered_review_mode(chat, "current changes");
}

fn next_text_turn(
    op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>,
) -> (String, Option<ModeKind>) {
    let Op::UserTurn {
        items,
        collaboration_mode,
        ..
    } = next_submit_op(op_rx)
    else {
        unreachable!("next_submit_op only returns user turns");
    };
    let [UserInput::Text { text, .. }] = items.as_slice() else {
        panic!("expected one text item, got {items:?}");
    };
    (
        text.clone(),
        collaboration_mode.map(|collaboration_mode| collaboration_mode.mode),
    )
}

fn complete_review_item_for_turn(chat: &mut ChatWidget, turn_id: &str, finding_count: usize) {
    chat.handle_server_notification(
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: chat.thread_id.expect("thread id").to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: 0,
            item: AppServerThreadItem::ExitedReviewMode {
                id: "review-end".to_string(),
                review: "finding".to_string(),
                finding_count,
            },
        }),
        /*replay_kind*/ None,
    );
}

async fn open_resolved_scope_picker(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    resolution: crate::review_scope::ReviewScopeResolution,
) {
    chat.open_review_popup();
    let (request_id, cwd, _) = next_scope_resolution(rx).await;
    assert!(chat.apply_review_scope_resolution(request_id, cwd, resolution));
}

async fn next_scope_resolution(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> (
    uuid::Uuid,
    PathBuf,
    crate::review_scope::ReviewScopeResolution,
) {
    loop {
        if let AppEvent::ReviewScopesResolved {
            request_id,
            cwd,
            resolution,
        } = rx.recv().await.expect("scope resolution event")
        {
            return (request_id, cwd, resolution);
        }
    }
}

fn configured_thread_session(thread_id: ThreadId) -> crate::session_state::ThreadSessionState {
    crate::session_state::ThreadSessionState {
        thread_id,
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "gpt-5".to_string(),
        model_provider_id: "openai".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: test_path_buf("/tmp/review-flow").abs(),
        runtime_workspace_roots: vec![test_path_buf("/tmp/review-flow").abs()],
        instruction_source_paths: Vec::new(),
        reasoning_effort: None,
        collaboration_mode: None,
        personality: None,
        message_history: None,
        network_proxy: None,
        rollout_path: None,
    }
}

struct FakeReviewScopeResolver {
    calls: Arc<Mutex<Vec<ThreadId>>>,
    result: Result<crate::review_scope::ReviewScopeResolution, String>,
}

impl crate::review_scope::ReviewScopeResolver for FakeReviewScopeResolver {
    fn resolve(
        &self,
        thread_id: ThreadId,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<crate::review_scope::ReviewScopeResolution, String>>
                + Send
                + '_,
        >,
    > {
        self.calls.lock().expect("calls lock").push(thread_id);
        Box::pin(std::future::ready(self.result.clone()))
    }
}
