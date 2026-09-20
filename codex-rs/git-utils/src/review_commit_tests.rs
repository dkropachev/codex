use std::collections::HashMap;
use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use codex_file_system::CopyOptions;
use codex_file_system::CreateDirectoryOptions;
use codex_file_system::ExecutorFileSystem;
use codex_file_system::ExecutorFileSystemFuture;
use codex_file_system::FileMetadata;
use codex_file_system::FileSystemReadStream;
use codex_file_system::FileSystemSandboxContext;
use codex_file_system::ReadDirectoryEntry;
use codex_file_system::RemoveOptions;
use codex_file_system::WalkOptions;
use codex_file_system::WalkOutcome;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::time::Duration;

use super::*;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;

#[tokio::test]
async fn commits_exact_add_and_preserves_dirty_state() {
    let repository = TestRepository::new();
    repository.write("staged.txt", "base staged\n");
    repository.write("unstaged.txt", "base unstaged\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    repository.write("staged.txt", "pre-existing staged\n");
    repository.git(&["add", "staged.txt"]);
    repository.write("unstaged.txt", "pre-existing unstaged\n");
    repository.write("untracked.txt", "pre-existing untracked\n");
    let original_head = repository.git(&["rev-parse", "HEAD"]);
    let original_status = repository.git(&["status", "--porcelain=v1", "--untracked-files=all"]);

    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let protected_paths = snapshot.protected_paths();
    assert!(protected_paths.contains(&root.join(".git/index").expect("index URI")));
    let path = root.join("review_fix.rs").expect("review fix URI");
    let changes = vec![ReviewFixFileChange::Add {
        path,
        content: "pub fn fixed() {}\n".to_string(),
    }];
    assert!(
        review_fix_snapshot_has_changes(runner.as_ref(), Arc::clone(&fs), &snapshot, &changes)
            .await
            .expect("detect exact change")
    );
    repository.write("review_fix.rs", "pub fn fixed() {}\n");
    let outcome = commit_review_fixes(
        runner,
        fs,
        &snapshot,
        &changes,
        "Apply verified review fixes",
    )
    .await
    .expect("commit review fixes");
    let ReviewFixCommitOutcome::Committed { commit_sha } = outcome else {
        panic!("expected a commit");
    };

    assert_eq!(repository.git(&["rev-parse", "HEAD"]), commit_sha);
    assert_eq!(repository.git(&["rev-parse", "HEAD^"]), original_head);
    assert_eq!(
        repository.git(&["show", "HEAD:review_fix.rs"]),
        "pub fn fixed() {}"
    );
    assert_eq!(
        repository.git(&["status", "--porcelain=v1", "--untracked-files=all"]),
        original_status
    );
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn commit_ignores_git_replace_objects() {
    let repository = TestRepository::new();
    repository.write("base.txt", "base\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "base"]);
    let base = repository.git(&["rev-parse", "HEAD"]);
    repository.write("replacement.txt", "replacement\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "replacement"]);
    let replacement = repository.git(&["rev-parse", "HEAD"]);
    repository.git(&["reset", "--hard", &base]);
    repository.git(&["replace", &base, &replacement]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let changes = vec![ReviewFixFileChange::Add {
        path: root.join("fixed.txt").expect("fixed URI"),
        content: "fixed\n".to_string(),
    }];
    repository.write("fixed.txt", "fixed\n");

    let outcome = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect("commit review fixes");

    assert!(matches!(outcome, ReviewFixCommitOutcome::Committed { .. }));
    assert_eq!(
        repository.git(&["ls-tree", "--name-only", "HEAD"]),
        "base.txt\nfixed.txt"
    );
}

#[tokio::test]
async fn committed_outcome_survives_temporary_file_cleanup_failure() {
    let repository = TestRepository::new();
    repository.write("base.txt", "base\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let runner = Arc::new(NativeTestRunner);
    let filesystem = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot = capture_review_fix_commit_snapshot(&*runner, filesystem.clone(), &root)
        .await
        .expect("snapshot");
    let path = root.join("fixed.txt").expect("fixed path");
    let changes = vec![ReviewFixFileChange::Add {
        path,
        content: "fixed\n".to_string(),
    }];
    repository.write("fixed.txt", "fixed\n");
    filesystem
        .fail_temporary_removes
        .store(true, Ordering::Relaxed);

    let outcome = commit_review_fixes(
        runner,
        filesystem.clone(),
        &snapshot,
        &changes,
        "Apply verified review fixes",
    )
    .await
    .expect("committed outcome");
    let ReviewFixCommitOutcome::Committed { commit_sha } = outcome else {
        panic!("expected committed outcome");
    };

    assert_eq!(repository.git(&["rev-parse", "HEAD"]), commit_sha);
    filesystem
        .fail_temporary_removes
        .store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn excludes_later_edit_to_the_same_file() {
    let repository = TestRepository::new();
    repository.write("shared.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("shared.txt").expect("shared URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+fixed\n".to_string(),
        move_path: None,
    }];

    repository.write("shared.txt", "fixed\n");
    repository.write("shared.txt", "later editor change\n");
    commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect("commit exact recorded change");

    assert_eq!(repository.git(&["show", "HEAD:shared.txt"]), "fixed");
    assert_eq!(
        std::fs::read_to_string(repository.path().join("shared.txt")).expect("read worktree"),
        "later editor change\n"
    );
    assert_eq!(repository.git(&["status", "--short"]), "M shared.txt");
    assert_no_temporary_files(repository.path());
}

#[cfg(unix)]
#[tokio::test]
async fn never_executes_configured_clean_filters() {
    use std::os::unix::fs::PermissionsExt;

    let repository = TestRepository::new();
    repository.write("tracked.txt", "before\n");
    repository.write(".gitattributes", "*.txt filter=review-test\n");
    let marker = repository.path().join("clean-filter-ran");
    let filter = repository.path().join("clean-filter.sh");
    std::fs::write(
        &filter,
        format!("#!/bin/sh\nprintf ran > '{}'\ncat\n", marker.display()),
    )
    .expect("write clean filter");
    let mut permissions = std::fs::metadata(&filter)
        .expect("filter metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&filter, permissions).expect("make filter executable");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    repository.git(&[
        "config",
        "filter.review-test.clean",
        filter.to_str().expect("UTF-8 path"),
    ]);
    let _ = std::fs::remove_file(&marker);

    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    assert!(!marker.exists(), "clean filter ran while snapshotting");
    let changes = vec![ReviewFixFileChange::Add {
        path: root.join("new.txt").expect("new file URI"),
        content: "exact content\n".to_string(),
    }];
    assert!(
        review_fix_snapshot_has_changes(runner.as_ref(), Arc::clone(&fs), &snapshot, &changes)
            .await
            .expect("verify change")
    );
    assert!(!marker.exists(), "clean filter ran while verifying");
    repository.write("new.txt", "exact content\n");
    commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect("commit without clean filter");

    assert!(!marker.exists(), "clean filter ran while committing");
    assert_eq!(repository.git(&["show", "HEAD:new.txt"]), "exact content");
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn preserves_nonoverlapping_staged_and_unstaged_edits_in_one_file() {
    let repository = TestRepository::new();
    let base = (1..=15)
        .map(|line| format!("base {line}\n"))
        .collect::<String>();
    repository.write("shared.txt", &base);
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let mut staged = base.lines().map(str::to_string).collect::<Vec<_>>();
    staged[0] = "pre-existing staged".to_string();
    repository.write("shared.txt", &with_newlines(&staged));
    repository.git(&["add", "shared.txt"]);
    let mut worktree = staged.clone();
    worktree[7] = "pre-existing unstaged".to_string();
    repository.write("shared.txt", &with_newlines(&worktree));
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("shared.txt").expect("shared URI"),
        unified_diff: "@@ -14,2 +14,2 @@\n base 14\n-base 15\n+review fix\n".to_string(),
        move_path: None,
    }];
    worktree[14] = "review fix".to_string();
    repository.write("shared.txt", &with_newlines(&worktree));

    commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect("commit review fix");

    let mut committed = base.lines().map(str::to_string).collect::<Vec<_>>();
    committed[14] = "review fix".to_string();
    assert_eq!(
        repository.git(&["show", "HEAD:shared.txt"]),
        with_newlines(&committed).trim()
    );
    let mut expected_index = staged;
    expected_index[14] = "review fix".to_string();
    assert_eq!(
        repository.git(&["show", ":shared.txt"]),
        with_newlines(&expected_index).trim()
    );
    assert_eq!(
        std::fs::read_to_string(repository.path().join("shared.txt")).expect("read shared file"),
        with_newlines(&worktree)
    );
}

#[tokio::test]
async fn preserves_index_flags_on_changed_paths() {
    let repository = TestRepository::new();
    repository.write("hidden.txt", "before\n");
    repository.write("skipped.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    repository.git(&["update-index", "--assume-unchanged", "hidden.txt"]);
    repository.git(&["update-index", "--skip-worktree", "skipped.txt"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let changes = ["hidden.txt", "skipped.txt"]
        .into_iter()
        .map(|path| ReviewFixFileChange::Update {
            path: root.join(path).expect("changed path URI"),
            unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
            move_path: None,
        })
        .collect::<Vec<_>>();
    repository.write("hidden.txt", "after\n");
    repository.write("skipped.txt", "after\n");

    commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fixes")
        .await
        .expect("commit flagged files");

    assert!(
        repository
            .git(&["ls-files", "-v", "hidden.txt"])
            .starts_with("h ")
    );
    assert!(
        repository
            .git(&["ls-files", "-v", "skipped.txt"])
            .starts_with("S ")
    );
}

#[tokio::test]
async fn applies_ordered_updates_and_moves() {
    let repository = TestRepository::new();
    repository.write("old.txt", "one\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let old = root.join("old.txt").expect("old URI");
    let new = root.join("new.txt").expect("new URI");
    let changes = vec![
        ReviewFixFileChange::Update {
            path: old.clone(),
            unified_diff: "@@ -1 +1 @@\n-one\n+two\n".to_string(),
            move_path: None,
        },
        ReviewFixFileChange::Update {
            path: old,
            unified_diff: "@@ -1 +1 @@\n-two\n+three\n".to_string(),
            move_path: Some(new),
        },
    ];
    std::fs::rename(
        repository.path().join("old.txt"),
        repository.path().join("new.txt"),
    )
    .expect("move worktree file");
    repository.write("new.txt", "three\n");

    commit_review_fixes(runner, fs, &snapshot, &changes, "Apply ordered fixes")
        .await
        .expect("commit ordered changes");

    assert_eq!(repository.git(&["show", "HEAD:new.txt"]), "three");
    assert_eq!(
        repository.git(&["ls-tree", "--name-only", "HEAD"]),
        "new.txt"
    );
}

#[tokio::test]
async fn delete_requires_the_recorded_content() {
    let repository = TestRepository::new();
    repository.write("delete.txt", "base\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let changes = vec![ReviewFixFileChange::Delete {
        path: root.join("delete.txt").expect("delete URI"),
        content: "different\n".to_string(),
    }];

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("mismatched deletion must fail");
    assert!(error.to_string().contains("does not match"));
    assert_eq!(repository.git(&["rev-list", "--count", "HEAD"]), "1");
}

#[tokio::test]
async fn an_index_lock_prevents_any_ref_change() {
    let repository = TestRepository::new();
    repository.write("tracked.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let head = repository.git(&["rev-parse", "HEAD"]);
    let index = std::fs::read(repository.path().join(".git/index")).expect("read index");
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("tracked.txt").expect("tracked URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("tracked.txt", "after\n");
    std::fs::write(repository.path().join(".git/index.lock"), "held").expect("hold index lock");

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("real index lock must block installation");
    assert!(error.to_string().contains("index"));
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(repository.path().join(".git/index")).expect("read index"),
        index
    );
    std::fs::remove_file(repository.path().join(".git/index.lock")).expect("release index lock");
}

#[tokio::test]
async fn update_ref_failure_rolls_back_the_real_index() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let native_runner = Arc::new(NativeTestRunner);
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot =
        capture_review_fix_commit_snapshot(native_runner.as_ref(), Arc::clone(&fs), &root)
            .await
            .expect("snapshot");
    let head = repository.git(&["rev-parse", "HEAD"]);
    let staged = repository.git(&["diff", "--cached", "--binary"]);
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");

    let error = commit_review_fixes(
        Arc::new(RejectUpdateRefRunner),
        fs,
        &snapshot,
        &changes,
        "Apply review fix",
    )
    .await
    .expect_err("update-ref must fail");
    assert!(error.to_string().contains("failed to update HEAD"));
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(repository.git(&["diff", "--cached", "--binary"]), staged);
}

#[tokio::test]
async fn target_probe_failure_after_ref_update_rolls_back_ref_and_index() -> Result<()> {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let native_runner = Arc::new(NativeTestRunner);
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot =
        capture_review_fix_commit_snapshot(native_runner.as_ref(), Arc::clone(&fs), &root)
            .await
            .expect("snapshot");
    let original_head = repository.git(&["rev-parse", "HEAD"]);
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");

    let error = commit_review_fixes(
        Arc::new(SuccessfulUpdateThenProbeErrorRunner {
            fail_probe: AtomicBool::new(false),
        }),
        fs,
        &snapshot,
        &changes,
        "Apply review fix",
    )
    .await
    .expect_err("target probe must fail the transaction");

    assert!(error.to_string().contains("consistent state"));
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), original_head);
    assert_eq!(repository.git(&["show", ":fix.txt"]), "before");
    assert_eq!(repository.git(&["diff", "--cached", "--binary"]), "");
    Ok(())
}

#[tokio::test]
async fn update_ref_failure_restores_unchanged_entries_and_preserves_index_races() {
    let repository = TestRepository::new();
    repository.write("first.txt", "first before\n");
    repository.write("second.txt", "second before\n");
    repository.write("other.txt", "other before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let native_runner = Arc::new(NativeTestRunner);
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot =
        capture_review_fix_commit_snapshot(native_runner.as_ref(), Arc::clone(&fs), &root)
            .await
            .expect("snapshot");
    let changes = vec![
        ReviewFixFileChange::Update {
            path: root.join("first.txt").expect("first URI"),
            unified_diff: "@@ -1 +1 @@\n-first before\n+first after\n".to_string(),
            move_path: None,
        },
        ReviewFixFileChange::Update {
            path: root.join("second.txt").expect("second URI"),
            unified_diff: "@@ -1 +1 @@\n-second before\n+second after\n".to_string(),
            move_path: None,
        },
    ];
    repository.write("first.txt", "first after\n");
    repository.write("second.txt", "second after\n");
    repository.write("other.txt", "other concurrent\n");

    let error = commit_review_fixes(
        Arc::new(IndexRaceRejectUpdateRefRunner),
        fs,
        &snapshot,
        &changes,
        "Apply review fix",
    )
    .await
    .expect_err("update-ref must fail");

    assert!(error.to_string().contains("failed to update HEAD"));
    assert_eq!(repository.git(&["show", ":first.txt"]), "first concurrent");
    assert_eq!(repository.git(&["show", ":second.txt"]), "second before");
    assert_eq!(repository.git(&["show", ":other.txt"]), "other concurrent");
}

#[tokio::test]
async fn concurrent_ref_advance_after_index_install_restores_the_index() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let branch = repository.git(&["symbolic-ref", "HEAD"]);
    let branch_name = repository.git(&["symbolic-ref", "--short", "HEAD"]);
    let original_head = repository.git(&["rev-parse", "HEAD"]);
    repository.git(&["checkout", "-q", "-b", "concurrent"]);
    repository.write("other.txt", "concurrent\n");
    repository.git(&["add", "other.txt"]);
    repository.git(&["commit", "-m", "concurrent"]);
    let concurrent_head = repository.git(&["rev-parse", "HEAD"]);
    repository.git(&["checkout", "-q", &branch_name]);

    let native_runner = Arc::new(NativeTestRunner);
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot =
        capture_review_fix_commit_snapshot(native_runner.as_ref(), Arc::clone(&fs), &root)
            .await
            .expect("snapshot");
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");
    let runner = Arc::new(HeadRaceRunner::before(vec![
        "update-ref".to_string(),
        branch,
        concurrent_head.clone(),
        original_head,
    ]));

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("concurrent ref update must win");

    assert!(error.to_string().contains("consistent state"));
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), concurrent_head);
    assert_eq!(repository.git(&["show", ":fix.txt"]), "before");
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn concurrent_branch_switch_never_receives_the_review_commit() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let original_branch = repository.git(&["symbolic-ref", "HEAD"]);
    let original_head = repository.git(&["rev-parse", "HEAD"]);
    repository.git(&["branch", "other"]);
    let (native_runner, fs, root, snapshot) = snapshot(&repository).await;
    drop(native_runner);
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");
    let runner = Arc::new(HeadRaceRunner::before(vec![
        "symbolic-ref".to_string(),
        "HEAD".to_string(),
        "refs/heads/other".to_string(),
    ]));

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("branch switch must abort the review commit");

    assert!(error.to_string().contains("consistent state"));
    assert_eq!(
        repository.git(&["symbolic-ref", "HEAD"]),
        "refs/heads/other"
    );
    assert_eq!(
        repository.git(&["rev-parse", "refs/heads/other"]),
        original_head
    );
    assert_eq!(
        repository.git(&["rev-parse", &original_branch]),
        original_head
    );
    assert_eq!(repository.git(&["show", ":fix.txt"]), "before");
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn branch_switch_after_ref_update_rolls_back_ref_and_index() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let original_branch = repository.git(&["symbolic-ref", "HEAD"]);
    let original_head = repository.git(&["rev-parse", "HEAD"]);
    repository.git(&["branch", "other"]);
    let (native_runner, fs, root, snapshot) = snapshot(&repository).await;
    drop(native_runner);
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");
    let runner = Arc::new(HeadRaceRunner::after(vec![
        "symbolic-ref".to_string(),
        "HEAD".to_string(),
        "refs/heads/other".to_string(),
    ]));

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("post-update branch switch must roll back");

    assert!(error.to_string().contains("consistent state"));
    assert_eq!(
        repository.git(&["symbolic-ref", "HEAD"]),
        "refs/heads/other"
    );
    assert_eq!(
        repository.git(&["rev-parse", "refs/heads/other"]),
        original_head
    );
    assert_eq!(
        repository.git(&["rev-parse", &original_branch]),
        original_head
    );
    assert_eq!(repository.git(&["show", ":fix.txt"]), "before");
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn detached_head_is_rejected_without_changing_the_index() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    repository.git(&["checkout", "-q", "--detach"]);
    let (runner, fs, root, snapshot) = snapshot(&repository).await;
    let head = repository.git(&["rev-parse", "HEAD"]);
    let index = std::fs::read(repository.path().join(".git/index")).expect("read index");
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");

    let error = commit_review_fixes(runner, fs, &snapshot, &changes, "Apply review fix")
        .await
        .expect_err("detached HEAD must fail closed");

    assert!(error.to_string().contains("detached HEAD"));
    assert_eq!(repository.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        std::fs::read(repository.path().join(".git/index")).expect("read index"),
        index
    );
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn cancellation_cannot_interrupt_the_real_index_ref_transaction() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let runner = Arc::new(BlockingUpdateRefRunner {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot = capture_review_fix_commit_snapshot(runner.as_ref(), Arc::clone(&fs), &root)
        .await
        .expect("snapshot");
    let changes = vec![ReviewFixFileChange::Update {
        path: root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\n".to_string(),
        move_path: None,
    }];
    repository.write("fix.txt", "after\n");
    let task_snapshot = snapshot.clone();
    let task_changes = changes.clone();
    let task = tokio::spawn(async move {
        commit_review_fixes(
            runner,
            fs,
            &task_snapshot,
            &task_changes,
            "Apply review fix",
        )
        .await
    });
    entered.notified().await;
    task.abort();
    let _ = task.await;
    release.notify_waiters();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if repository.git(&["show", "HEAD:fix.txt"]) == "after" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shielded transaction should finish");
    assert_eq!(repository.git(&["status", "--short"]), "");
    assert_no_temporary_files(repository.path());
}

#[tokio::test]
async fn cancelling_snapshot_capture_cleans_temporary_git_files() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let entered = Arc::new(Notify::new());
    let runner = Arc::new(BlockingWriteTreeRunner {
        entered: Arc::clone(&entered),
    });
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let task = tokio::spawn(async move {
        capture_review_fix_commit_snapshot(runner.as_ref(), fs, &root).await
    });
    entered.notified().await;

    task.abort();
    let _ = task.await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if temporary_files(repository.path()).is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("temporary snapshot files should be cleaned");
}

#[tokio::test]
async fn rejects_outside_paths_and_injected_patch_headers() {
    let repository = TestRepository::new();
    repository.write("fix.txt", "before\n");
    repository.git(&["add", "."]);
    repository.git(&["commit", "-m", "initial"]);
    let (runner, fs, _root, snapshot) = snapshot(&repository).await;
    let outside = vec![ReviewFixFileChange::Add {
        path: PathUri::from_host_native_path(
            repository.path().parent().expect("parent").join("out"),
        )
        .expect("outside URI"),
        content: "outside\n".to_string(),
    }];
    let error = commit_review_fixes(
        Arc::clone(&runner),
        Arc::clone(&fs),
        &snapshot,
        &outside,
        "Apply review fix",
    )
    .await
    .expect_err("outside path must fail");
    assert!(error.to_string().contains("outside the repository"));

    let injected = vec![ReviewFixFileChange::Update {
        path: snapshot.repository_root.join("fix.txt").expect("fix URI"),
        unified_diff: "@@ -1 +1 @@\n-before\n+after\ndiff --git a/out b/out\n".to_string(),
        move_path: None,
    }];
    let error = commit_review_fixes(runner, fs, &snapshot, &injected, "Apply review fix")
        .await
        .expect_err("extra patch must fail");
    assert!(error.to_string().contains("outside a unified diff hunk"));
}

#[tokio::test]
async fn windows_snapshot_uses_remote_path_uris_without_staging_worktree() {
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TREE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const INDEX_TREE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const EMPTY_TREE: &str = "dddddddddddddddddddddddddddddddddddddddd";
    let root = PathUri::parse("file:///C:/workspace").expect("root URI");
    let index = PathUri::parse("file:///C:/workspace/.git/index").expect("index URI");
    let fs = Arc::new(MemoryFileSystem::with_file(index, b"raw-index".to_vec()));
    let runner = ScriptedRunner::new(
        root.clone(),
        vec![
            success(&["rev-parse", "--verify", "HEAD^{commit}"], HEAD),
            success(&["rev-parse", "--verify", "HEAD^{tree}"], TREE),
            success(&["symbolic-ref", "-q", "HEAD"], "refs/heads/main"),
            success(&["rev-parse", "--git-path", "index"], ".git\\index"),
            success_exact(&["ls-files", "-v", "-z"], ""),
            success(&["mktree"], EMPTY_TREE),
            success(&["write-tree"], INDEX_TREE),
            success(&["rev-parse", "--verify", "HEAD^{commit}"], HEAD),
            success(&["symbolic-ref", "-q", "HEAD"], "refs/heads/main"),
        ],
    );

    let snapshot = capture_review_fix_commit_snapshot(&runner, fs.clone(), &root)
        .await
        .expect("capture Windows snapshot");

    assert_eq!(snapshot.index_tree, INDEX_TREE);
    assert!(
        runner
            .seen()
            .iter()
            .all(|command| !command.argv().iter().any(|arg| arg == "add"))
    );
    assert!(
        fs.files()
            .keys()
            .all(|path| !path.to_string().contains(TEMP_FILE_PREFIX))
    );
    runner.assert_exhausted();
}

async fn snapshot(
    repository: &TestRepository,
) -> (
    Arc<NativeTestRunner>,
    Arc<dyn ExecutorFileSystem>,
    PathUri,
    ReviewFixCommitSnapshot,
) {
    let runner = Arc::new(NativeTestRunner);
    let fs: Arc<dyn ExecutorFileSystem> = Arc::new(NativeTestFileSystem::default());
    let root = PathUri::from_host_native_path(repository.path()).expect("repository URI");
    let snapshot = capture_review_fix_commit_snapshot(runner.as_ref(), Arc::clone(&fs), &root)
        .await
        .expect("capture review fix snapshot");
    (runner, fs, root, snapshot)
}

fn with_newlines(lines: &[String]) -> String {
    format!("{}\n", lines.join("\n"))
}

struct NativeTestRunner;

impl ReviewCommandRunner for NativeTestRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        run_native(command)
    }
}

fn run_native(command: ReviewCommand) -> Result<ReviewCommandOutput> {
    let cwd = command.cwd().to_abs_path()?;
    let (program, args) = command.argv().split_first().context("empty test command")?;
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd.as_path())
        .envs(command.env_vars())
        .env_remove("GH_REPO")
        .output()?;
    Ok(ReviewCommandOutput {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

struct RejectUpdateRefRunner;

impl ReviewCommandRunner for RejectUpdateRefRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        if command_args(&command).first().map(String::as_str) == Some("update-ref") {
            return Ok(ReviewCommandOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "injected update-ref failure".to_string(),
            });
        }
        run_native(command)
    }
}

struct IndexRaceRejectUpdateRefRunner;

impl ReviewCommandRunner for IndexRaceRejectUpdateRefRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        if command_args(&command).first().map(String::as_str) == Some("update-ref") {
            let cwd = command.cwd().to_abs_path()?;
            std::fs::write(cwd.join("first.txt"), "first concurrent\n")?;
            run_side_effect_git(
                &command,
                &[
                    "add".to_string(),
                    "first.txt".to_string(),
                    "other.txt".to_string(),
                ],
            )?;
            return Ok(ReviewCommandOutput {
                exit_code: 1,
                stdout: String::new(),
                stderr: "injected update-ref failure".to_string(),
            });
        }
        run_native(command)
    }
}

struct SuccessfulUpdateThenProbeErrorRunner {
    fail_probe: AtomicBool,
}

impl ReviewCommandRunner for SuccessfulUpdateThenProbeErrorRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let args = command_args(&command);
        if args.first().map(String::as_str) == Some("update-ref") {
            let output = run_native(command)?;
            if output.exit_code == 0
                && args.get(2).map(String::as_str) == Some("review: apply verified fixes")
            {
                self.fail_probe.store(true, Ordering::Relaxed);
            }
            return Ok(output);
        }
        if self.fail_probe.load(Ordering::Relaxed)
            && matches!(args.as_slice(), [command, verify, quiet, ..] if command == "rev-parse" && verify == "--verify" && quiet == "--quiet")
        {
            self.fail_probe.store(false, Ordering::Relaxed);
            anyhow::bail!("injected target probe failure");
        }
        run_native(command)
    }
}

#[derive(Clone, Copy)]
enum RaceTiming {
    Before,
    After,
}

struct HeadRaceRunner {
    timing: RaceTiming,
    command: Mutex<Option<Vec<String>>>,
}

impl HeadRaceRunner {
    fn before(command: Vec<String>) -> Self {
        Self {
            timing: RaceTiming::Before,
            command: Mutex::new(Some(command)),
        }
    }

    fn after(command: Vec<String>) -> Self {
        Self {
            timing: RaceTiming::After,
            command: Mutex::new(Some(command)),
        }
    }
}

impl ReviewCommandRunner for HeadRaceRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let race_command = if is_review_ref_update(&command) {
            self.command.lock().expect("race command lock").take()
        } else {
            None
        };
        if matches!(self.timing, RaceTiming::Before)
            && let Some(race_command) = race_command.as_ref()
        {
            run_side_effect_git(&command, race_command)?;
        }
        let output = run_native(command.clone())?;
        if matches!(self.timing, RaceTiming::After)
            && let Some(race_command) = race_command.as_ref()
        {
            run_side_effect_git(&command, race_command)?;
        }
        Ok(output)
    }
}

fn is_review_ref_update(command: &ReviewCommand) -> bool {
    let args = command_args(command);
    matches!(
        args.as_slice(),
        [command, flag, message, ..]
            if command == "update-ref"
                && flag == "-m"
                && message == "review: apply verified fixes"
    )
}

fn run_side_effect_git(command: &ReviewCommand, args: &[String]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(command.cwd().to_abs_path()?.as_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "concurrent git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

struct BlockingUpdateRefRunner {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct BlockingWriteTreeRunner {
    entered: Arc<Notify>,
}

impl ReviewCommandRunner for BlockingWriteTreeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        if command_args(&command).first().map(String::as_str) == Some("write-tree") {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        run_native(command)
    }
}

impl ReviewCommandRunner for BlockingUpdateRefRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        if command_args(&command).first().map(String::as_str) == Some("update-ref") {
            self.entered.notify_waiters();
            self.release.notified().await;
        }
        run_native(command)
    }
}

struct TestRepository {
    directory: TempDir,
}

impl TestRepository {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let repository = Self { directory };
        repository.git(&["init", "--quiet"]);
        repository.git(&["config", "user.name", "Review Test"]);
        repository.git(&["config", "user.email", "review@example.com"]);
        repository
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.path().join(path), contents).expect("write repository file");
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

fn assert_no_temporary_files(repository: &Path) {
    assert_eq!(temporary_files(repository), Vec::<String>::new());
}

fn temporary_files(repository: &Path) -> Vec<String> {
    std::fs::read_dir(repository.join(".git"))
        .expect("read Git directory")
        .map(|entry| {
            entry
                .expect("Git entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with(TEMP_FILE_PREFIX))
        .collect()
}

fn command_args(command: &ReviewCommand) -> Vec<String> {
    command.argv()[5..].to_vec()
}

struct ExpectedCommand {
    args: Vec<String>,
    output: ReviewCommandOutput,
}

fn success(args: &[&str], stdout: &str) -> ExpectedCommand {
    ExpectedCommand {
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code: 0,
            stdout: format!("{stdout}\n"),
            stderr: String::new(),
        },
    }
}

fn success_exact(args: &[&str], stdout: &str) -> ExpectedCommand {
    ExpectedCommand {
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        },
    }
}

struct ScriptedRunner {
    root: PathUri,
    expected: Mutex<VecDeque<ExpectedCommand>>,
    seen: Mutex<Vec<ReviewCommand>>,
}

impl ScriptedRunner {
    fn new(root: PathUri, expected: Vec<ExpectedCommand>) -> Self {
        Self {
            root,
            expected: Mutex::new(expected.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<ReviewCommand> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn assert_exhausted(&self) {
        assert_eq!(self.expected.lock().expect("expected lock").len(), 0);
    }
}

impl ReviewCommandRunner for ScriptedRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        assert_eq!(command.cwd(), &self.root);
        assert_eq!(
            &command.argv()[..5],
            [
                "git",
                "-c",
                "core.hooksPath=NUL",
                "-c",
                "core.fsmonitor=false"
            ]
        );
        self.seen.lock().expect("seen lock").push(command.clone());
        let expected = self
            .expected
            .lock()
            .expect("expected lock")
            .pop_front()
            .expect("unexpected command");
        assert_eq!(command_args(&command), expected.args);
        Ok(expected.output)
    }
}

#[derive(Default)]
struct MemoryFileSystem {
    files: Mutex<HashMap<PathUri, Vec<u8>>>,
}

impl MemoryFileSystem {
    fn with_file(path: PathUri, contents: Vec<u8>) -> Self {
        Self {
            files: Mutex::new(HashMap::from([(path, contents)])),
        }
    }

    fn files(&self) -> HashMap<PathUri, Vec<u8>> {
        self.files.lock().expect("files lock").clone()
    }
}

impl ExecutorFileSystem for MemoryFileSystem {
    fn canonicalize<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        unimplemented!()
    }
    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move {
            self.files
                .lock()
                .expect("files lock")
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.to_string()))
        })
    }
    fn read_file_stream<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        unimplemented!()
    }
    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            self.files
                .lock()
                .expect("files lock")
                .insert(path.clone(), contents);
            Ok(())
        })
    }
    fn create_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: CreateDirectoryOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }
    fn get_metadata<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        unimplemented!()
    }
    fn read_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        unimplemented!()
    }
    fn walk<'a>(
        &'a self,
        _: &'a PathUri,
        _: WalkOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        unimplemented!()
    }
    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        _: RemoveOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            self.files.lock().expect("files lock").remove(path);
            Ok(())
        })
    }
    fn copy<'a>(
        &'a self,
        _: &'a PathUri,
        _: &'a PathUri,
        _: CopyOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }
}

#[derive(Default)]
struct NativeTestFileSystem {
    fail_temporary_removes: AtomicBool,
}

impl ExecutorFileSystem for NativeTestFileSystem {
    fn canonicalize<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, PathUri> {
        unimplemented!()
    }
    fn read_file<'a>(
        &'a self,
        path: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<u8>> {
        Box::pin(async move { std::fs::read(path.to_abs_path()?.as_path()) })
    }
    fn read_file_stream<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileSystemReadStream> {
        unimplemented!()
    }
    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move { std::fs::write(path.to_abs_path()?.as_path(), contents) })
    }
    fn create_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: CreateDirectoryOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }
    fn get_metadata<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        unimplemented!()
    }
    fn read_directory<'a>(
        &'a self,
        _: &'a PathUri,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, Vec<ReadDirectoryEntry>> {
        unimplemented!()
    }
    fn walk<'a>(
        &'a self,
        _: &'a PathUri,
        _: WalkOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, WalkOutcome> {
        unimplemented!()
    }
    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            if self.fail_temporary_removes.load(Ordering::Relaxed)
                && path
                    .basename()
                    .is_some_and(|name| name.starts_with(TEMP_FILE_PREFIX))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected temporary-file cleanup failure",
                ));
            }
            match std::fs::remove_file(path.to_abs_path()?.as_path()) {
                Ok(()) => Ok(()),
                Err(error) if options.force && error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        })
    }
    fn copy<'a>(
        &'a self,
        _: &'a PathUri,
        _: &'a PathUri,
        _: CopyOptions,
        _: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        unimplemented!()
    }
}
