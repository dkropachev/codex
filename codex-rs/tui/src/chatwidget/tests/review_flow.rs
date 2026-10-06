use super::*;
use codex_app_server_protocol::ReviewScopeCommit;
use codex_app_server_protocol::ReviewTarget;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

const REVIEW_TURN_ID: &str = "review-turn";

#[tokio::test]
async fn review_commit_picker_uses_resolved_checkout_commits() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5")).await;
    let commits = vec![
        ReviewScopeCommit {
            sha: "1111111deadbeef".to_string(),
            subject: "Add new feature X".to_string(),
        },
        ReviewScopeCommit {
            sha: "2222222cafebabe".to_string(),
            subject: "Fix bug Y".to_string(),
        },
    ];
    open_resolved_scope_picker(
        &mut chat,
        &mut rx,
        crate::review_scope::ReviewScopeResolution {
            commits: commits.clone(),
            ..Default::default()
        },
    )
    .await;

    let cwd = chat.config.cwd.to_path_buf();
    chat.show_review_commit_picker(chat.thread_id, &cwd);
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(popup.contains("Add new feature X"));
    assert!(popup.contains("Fix bug Y"));
    assert!(!popup.contains("ago"));

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::StartReportReview {
            target: ReviewTarget::Commit { sha, title },
            ..
        }) if sha == commits[0].sha && title.as_deref() == Some(commits[0].subject.as_str())
    );
}

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
        AppEvent::StartReportReview {
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
            commits: Vec::new(),
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_pull_request_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReportReview {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("pull request scope selection")
    else {
        panic!("expected review scope selection event");
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
            commits: Vec::new(),
        },
    )
    .await;

    assert_chatwidget_snapshot!(
        "review_scope_default_branch_picker",
        render_bottom_popup(&chat, /*width*/ 80)
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let target = loop {
        if let AppEvent::StartReportReview { target, .. } =
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
        if let AppEvent::StartReportReview { target, .. } =
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
            commits: Vec::new(),
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
        if let AppEvent::StartReportReview { target, .. } =
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
        if let AppEvent::StartReportReview { target, .. } =
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
            commits: Vec::new(),
        },
    )
    .await;
    chat.show_review_branch_picker(chat.thread_id, &cwd);

    assert!(!render_bottom_popup(&chat, /*width*/ 80).contains("Pull request base"));
}

#[tokio::test]
async fn report_review_submits_review_op() {
    let target = ReviewTarget::Custom {
        instructions: "check regressions".to_string(),
    };
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    chat.start_review_for_thread(thread_id, cwd, target.clone());
    assert_matches!(
        op_rx.try_recv(),
        Ok(Op::Review { target: event_target }) if event_target == target
    );
}

#[tokio::test]
async fn pending_review_blocks_duplicate_request_and_can_be_retried() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();

    for _ in 0..2 {
        chat.start_review_for_thread(thread_id, cwd.clone(), ReviewTarget::UncommittedChanges);
    }
    assert_matches!(op_rx.try_recv(), Ok(Op::Review { .. }));
    assert_no_submit_op(&mut op_rx);

    chat.clear_pending_review();
    chat.start_review_for_thread(thread_id, cwd, ReviewTarget::UncommittedChanges);
    assert_matches!(op_rx.try_recv(), Ok(Op::Review { .. }));
}

#[tokio::test]
async fn report_review_never_starts_a_follow_up_turn() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    begin_review(&mut chat, &mut op_rx);

    handle_exited_review_mode_with_findings(&mut chat, /*finding_count*/ 2);
    handle_turn_completed(&mut chat, REVIEW_TURN_ID, /*duration_ms*/ None);

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn stale_scope_selection_does_not_start_review_on_new_thread() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    chat.thread_id = Some(ThreadId::new());

    chat.start_review_for_thread(thread_id, cwd, ReviewTarget::UncommittedChanges);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn stale_scope_selection_does_not_start_review_after_cwd_change() {
    let (mut chat, _rx, mut op_rx) = review_chat().await;
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    chat.config.cwd = test_path_buf("/tmp/other-review-cwd").abs();

    chat.start_review_for_thread(thread_id, cwd, ReviewTarget::UncommittedChanges);

    assert_matches!(op_rx.try_recv(), Err(TryRecvError::Empty));
}

#[tokio::test]
async fn scope_selection_does_not_start_review_after_cwd_change() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    chat.config.cwd = test_path_buf("/tmp/other-review-scope").abs();

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReportReview {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review scope selection event")
    else {
        panic!("expected review scope selection event");
    };
    chat.start_review_for_thread(thread_id, cwd, target);

    assert_no_submit_op(&mut op_rx);
}

#[tokio::test]
async fn scope_selection_does_not_start_review_after_same_cwd_thread_change() {
    let (mut chat, mut rx, mut op_rx) = review_chat().await;
    open_resolved_scope_picker(&mut chat, &mut rx, Default::default()).await;
    let next_thread = ThreadId::new();
    // Leave the picker intact to exercise its originating-thread guard directly; the normal
    // thread-session path may additionally dismiss or reset transient views.
    chat.thread_id = Some(next_thread);

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let AppEvent::StartReportReview {
        thread_id,
        cwd,
        target,
    } = rx.try_recv().expect("review scope selection event")
    else {
        panic!("expected review scope selection event");
    };
    chat.start_review_for_thread(thread_id, cwd, target);

    assert_eq!(chat.thread_id, Some(next_thread));
    assert_no_submit_op(&mut op_rx);
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
    (chat, rx, op_rx)
}

fn begin_review(chat: &mut ChatWidget, op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) {
    chat.start_review_for_thread(
        chat.thread_id,
        chat.config.cwd.to_path_buf(),
        ReviewTarget::UncommittedChanges,
    );
    assert_matches!(
        op_rx.try_recv(),
        Ok(Op::Review {
            target: ReviewTarget::UncommittedChanges
        })
    );
    handle_turn_started(chat, REVIEW_TURN_ID);
    handle_entered_review_mode(chat, "current changes");
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
