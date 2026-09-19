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
async fn review_scope_request_failure_selects_whole_repository() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.review_scope_resolver = Some(Arc::new(FakeReviewScopeResolver {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Err("environment unavailable".to_string()),
    }));

    chat.open_review_popup();
    let (request_id, cwd, resolution) = next_scope_resolution(&mut rx).await;

    assert_eq!(
        resolution.error.as_deref(),
        Some("Could not detect Git review scopes.")
    );
    assert!(chat.apply_review_scope_resolution(request_id, cwd, resolution));
    assert_chatwidget_snapshot!(
        "review_scope_git_error_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(
        rx.recv().await.expect("action event"),
        AppEvent::OpenReviewVerificationPicker {
            thread_id: _,
            cwd: _,
            target: ReviewTarget::WholeRepository
        }
    );
}

#[tokio::test]
async fn legacy_server_git_failure_selects_custom_review_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            whole_repository_available: false,
            error: Some("Could not detect Git review scopes.".to_string()),
            ..Default::default()
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_legacy_server_git_error",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_scope_pull_request_picker_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            review_execution_available: true,
            review_unavailable_reason: None,
            fix_execution_available: true,
            fix_unavailable_reason: None,
            double_check_available: true,
            whole_repository_available: true,
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
            has_uncommitted_changes: true,
            commits: vec![review_scope_commit()],
            error: None,
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_pull_request_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewVerificationPicker {
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
            review_execution_available: true,
            review_unavailable_reason: None,
            fix_execution_available: true,
            fix_unavailable_reason: None,
            double_check_available: true,
            whole_repository_available: true,
            pull_request: None,
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/origin/main".to_string()),
            current_branch: Some("feature/review".to_string()),
            branches: vec![
                "refs/remotes/origin/main".to_string(),
                "refs/heads/feature/review".to_string(),
            ],
            has_uncommitted_changes: true,
            commits: vec![review_scope_commit()],
            error: None,
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_default_branch_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewVerificationPicker { target, .. } =
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
async fn review_scope_clean_repository_picker_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;

    assert_chatwidget_snapshot!(
        "review_scope_clean_repository_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::OpenReviewVerificationPicker { target, .. } =
            rx.try_recv().expect("scope selection event")
        {
            break target;
        }
    };
    assert_eq!(target, ReviewTarget::WholeRepository);
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
            ..Default::default()
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
        if let AppEvent::OpenReviewVerificationPicker { target, .. } =
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
        if let AppEvent::OpenReviewVerificationPicker { target, .. } =
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
            ..Default::default()
        },
    )
    .await;
    chat.show_review_branch_picker(chat.thread_id, &cwd);

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Pull request base"));
}

#[tokio::test]
async fn review_action_picker_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    mark_review_commit_available(&mut chat);

    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
        ReviewVerification::SinglePass,
    );

    assert_chatwidget_snapshot!(
        "review_action_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_action_picker_hides_commit_when_git_is_unavailable_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd.clone(),
        crate::review_scope::ReviewScopeResolution {
            error: Some("Git detection failed".to_string()),
            ..Default::default()
        },
    );

    chat.show_review_action_picker(
        chat.thread_id,
        cwd,
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
    );

    let rendered = render_bottom_popup(&chat, /*width*/ 80);
    assert!(!rendered.contains("Fix findings + commit"));
    assert_chatwidget_snapshot!("review_action_picker_without_git", rendered);
}

#[tokio::test]
async fn review_action_picker_hides_fix_when_default_mode_is_unavailable_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    mark_review_commit_available(&mut chat);
    let params = chat.review_action_picker_params(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
        /*commit_available*/ true,
        super::super::review_settings_popups::ReviewFixAvailability::Unavailable,
    );
    chat.bottom_pane.show_selection_view(params);

    let rendered = render_bottom_popup(&chat, /*width*/ 80);
    assert!(!rendered.contains("Fix findings"));
    assert_chatwidget_snapshot!("review_action_picker_report_only", rendered);
}

#[tokio::test]
async fn review_action_picker_hides_fix_when_server_permissions_reject_it() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd.clone(),
        crate::review_scope::ReviewScopeResolution {
            fix_execution_available: false,
            fix_unavailable_reason: Some("Fix is unavailable.".to_string()),
            ..Default::default()
        },
    );

    chat.show_review_action_picker(
        chat.thread_id,
        cwd,
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
    );

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Fix findings"));
}

#[tokio::test]
async fn review_action_picker_shows_server_execution_unavailability_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd.clone(),
        crate::review_scope::ReviewScopeResolution {
            review_execution_available: false,
            review_unavailable_reason: Some(
                "Review requires Windows sandboxing for the selected local executor.".to_string(),
            ),
            ..Default::default()
        },
    );

    chat.show_review_action_picker(
        chat.thread_id,
        cwd,
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
    );

    assert_chatwidget_snapshot!(
        "review_action_picker_unavailable",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_action_picker_hides_commit_at_detached_head_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd.clone(),
        crate::review_scope::ReviewScopeResolution {
            current_branch: None,
            commits: vec![review_scope_commit()],
            ..Default::default()
        },
    );

    chat.show_review_action_picker(
        chat.thread_id,
        cwd,
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
    );

    let rendered = render_bottom_popup(&chat, /*width*/ 80);
    assert!(!rendered.contains("Fix findings + commit"));
    assert_chatwidget_snapshot!("review_action_picker_detached_head", rendered);
}

#[tokio::test]
async fn custom_review_silently_resolves_fix_commit_availability() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = Some(ThreadId::new());
    chat.thread_id = thread_id;
    chat.review_scope_resolver = Some(Arc::new(FakeReviewScopeResolver {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(crate::review_scope::ReviewScopeResolution {
            current_branch: Some("main".to_string()),
            commits: vec![review_scope_commit()],
            ..Default::default()
        }),
    }));
    let cwd = chat.config.cwd.to_path_buf();
    let target = ReviewTarget::Custom {
        instructions: "check regressions".to_string(),
    };

    chat.show_review_action_picker(
        thread_id,
        cwd.clone(),
        target.clone(),
        ReviewVerification::SinglePass,
    );
    let event = loop {
        let event = rx.recv().await.expect("action scope event");
        if matches!(event, AppEvent::ReviewActionScopeResolved { .. }) {
            break event;
        }
    };
    let AppEvent::ReviewActionScopeResolved {
        request_id,
        resolution,
        ..
    } = event
    else {
        unreachable!();
    };
    chat.apply_review_action_scope_resolution(
        request_id,
        thread_id,
        cwd,
        target,
        ReviewVerification::SinglePass,
        resolution,
    );

    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Fix findings + commit"));
}

#[tokio::test]
async fn dismissed_review_action_resolution_does_not_reopen_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = Some(ThreadId::new());
    chat.thread_id = thread_id;
    chat.review_scope_resolver = Some(Arc::new(FakeReviewScopeResolver {
        calls: Arc::new(Mutex::new(Vec::new())),
        result: Ok(crate::review_scope::ReviewScopeResolution {
            current_branch: Some("main".to_string()),
            commits: vec![review_scope_commit()],
            ..Default::default()
        }),
    }));
    let cwd = chat.config.cwd.to_path_buf();
    let target = ReviewTarget::Custom {
        instructions: "check regressions".to_string(),
    };

    chat.show_review_action_picker(
        thread_id,
        cwd.clone(),
        target.clone(),
        ReviewVerification::SinglePass,
    );
    let event = loop {
        let event = rx.recv().await.expect("action scope event");
        if matches!(event, AppEvent::ReviewActionScopeResolved { .. }) {
            break event;
        }
    };
    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    let dismissed = render_bottom_popup(&chat, /*width*/ 80);
    let AppEvent::ReviewActionScopeResolved {
        request_id,
        resolution,
        ..
    } = event
    else {
        unreachable!();
    };
    chat.apply_review_action_scope_resolution(
        request_id,
        thread_id,
        cwd,
        target,
        ReviewVerification::SinglePass,
        resolution,
    );

    assert_eq!(render_bottom_popup(&chat, /*width*/ 80), dismissed);
}

#[tokio::test]
async fn review_verification_picker_defaults_to_single_pass_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    let target = ReviewTarget::WholeRepository;

    chat.show_review_verification_picker(thread_id, cwd.clone(), target.clone());

    assert_chatwidget_snapshot!(
        "review_verification_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::OpenReviewActionPicker {
            thread_id: event_thread_id,
            cwd: event_cwd,
            target: event_target,
            verification: ReviewVerification::SinglePass,
        }) if event_thread_id == thread_id && event_cwd == cwd && event_target == target
    );
}

#[tokio::test]
async fn review_verification_picker_hides_double_check_for_older_server_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd.clone(),
        crate::review_scope::ReviewScopeResolution {
            double_check_available: false,
            ..Default::default()
        },
    );

    chat.show_review_verification_picker(chat.thread_id, cwd, ReviewTarget::WholeRepository);

    assert_chatwidget_snapshot!(
        "review_verification_picker_single_pass",
        render_bottom_popup(&chat, /*width*/ 80)
    );
}

#[tokio::test]
async fn review_verification_picker_emits_double_check() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    let target = ReviewTarget::WholeRepository;
    chat.show_review_verification_picker(thread_id, cwd.clone(), target.clone());

    chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::OpenReviewActionPicker {
            thread_id: event_thread_id,
            cwd: event_cwd,
            target: event_target,
            verification: ReviewVerification::DoubleCheck,
        }) if event_thread_id == thread_id && event_cwd == cwd && event_target == target
    );
}

#[tokio::test]
async fn review_action_picker_emits_fix_choices_with_all_settings() {
    let target = ReviewTarget::Custom {
        instructions: "check regressions".to_string(),
    };
    for (down_presses, expected_action) in [(1, ReviewAction::Fix), (2, ReviewAction::FixAndCommit)]
    {
        let (mut chat, mut rx, _op_rx) = review_chat().await;
        mark_review_commit_available(&mut chat);
        chat.review_scope_resolver = None;
        let thread_id = chat.thread_id;
        let cwd = chat.config.cwd.to_path_buf();
        chat.show_review_action_picker(
            thread_id,
            cwd.clone(),
            target.clone(),
            ReviewVerification::DoubleCheck,
        );
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
                verification,
                action,
            }) if event_thread_id == thread_id
                && event_cwd == cwd
                && event_target == target
                && verification == ReviewVerification::DoubleCheck
                && action == expected_action
        );
    }
}

#[tokio::test]
async fn fix_action_is_part_of_review_request_and_never_starts_a_follow_up_turn() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    chat.start_review_for_thread(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::WholeRepository,
        ReviewVerification::DoubleCheck,
        ReviewAction::Fix,
    );

    assert_matches!(
        op_rx.try_recv(),
        Ok(Op::Review {
            target: ReviewTarget::WholeRepository,
            verification: ReviewVerification::DoubleCheck,
            action: ReviewAction::Fix,
        })
    );
    handle_turn_started(&mut chat, REVIEW_TURN_ID);
    handle_entered_review_mode(&mut chat, "whole repository");
    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 1);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn review_without_active_thread_does_not_submit_or_mark_busy() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    chat.thread_id = None;

    chat.start_review_for_thread(
        /*thread_id*/ None,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::WholeRepository,
        ReviewVerification::SinglePass,
        ReviewAction::Report,
    );

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn stale_action_picker_selection_does_not_start_review_on_new_thread() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
        ReviewVerification::SinglePass,
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReview {
        thread_id,
        cwd,
        target,
        verification,
        action,
    } = rx.try_recv().expect("review action event")
    else {
        panic!("expected review action event");
    };
    chat.thread_id = Some(ThreadId::new());

    chat.start_review_for_thread(thread_id, cwd, target, verification, action);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn stale_action_picker_selection_does_not_start_review_after_cwd_change() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    chat.show_review_action_picker(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
        ReviewVerification::SinglePass,
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReview {
        thread_id,
        cwd,
        target,
        verification,
        action,
    } = rx.try_recv().expect("review action event")
    else {
        panic!("expected review action event");
    };
    chat.config.cwd = test_path_buf("/tmp/other-review-cwd").abs();

    chat.start_review_for_thread(thread_id, cwd, target, verification, action);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn scope_selection_does_not_open_verification_picker_after_cwd_change() {
    let (mut chat, mut rx, _op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    chat.config.cwd = test_path_buf("/tmp/other-review-scope").abs();

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewVerificationPicker {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review verification picker event")
    else {
        panic!("expected review verification picker event");
    };
    chat.show_review_verification_picker(thread_id, cwd, target);

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Choose review verification"));
}

#[tokio::test]
async fn scope_selection_does_not_open_verification_picker_after_same_cwd_thread_change() {
    let (mut chat, mut rx, _op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    let next_thread = ThreadId::new();
    // Leave the picker intact to exercise its originating-thread guard directly; the normal
    // thread-session path may additionally dismiss or reset transient views.
    chat.thread_id = Some(next_thread);

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::OpenReviewVerificationPicker {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review verification picker event")
    else {
        panic!("expected review verification picker event");
    };
    chat.show_review_verification_picker(thread_id, cwd, target);

    assert_eq!(chat.thread_id, Some(next_thread));
    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Choose review verification"));
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
    mark_review_commit_available(&mut chat);
    (chat, rx, op_rx)
}

fn review_scope_commit() -> ReviewScopeCommit {
    ReviewScopeCommit {
        sha: "abc1234".to_string(),
        title: "Example commit".to_string(),
    }
}

fn mark_review_commit_available(chat: &mut ChatWidget) {
    let cwd = chat.config.cwd.to_path_buf();
    let request_id = chat.review.begin_scope_resolution(cwd.clone());
    chat.review.set_scope_resolution(
        request_id,
        cwd,
        crate::review_scope::ReviewScopeResolution {
            review_execution_available: true,
            review_unavailable_reason: None,
            fix_execution_available: true,
            fix_unavailable_reason: None,
            double_check_available: true,
            whole_repository_available: true,
            current_branch: Some("main".to_string()),
            commits: vec![review_scope_commit()],
            ..Default::default()
        },
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
