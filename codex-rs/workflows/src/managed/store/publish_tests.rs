#![allow(clippy::expect_used)]

use std::fs;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::super::prepare::tests::store;
use super::super::prepare::tests::verified_release;
use super::super::receipt::ManagedWorkflowReceipt;
use super::super::receipt::read_receipt;
use super::*;

fn prepared<'a>(
    store: &'a ManagedWorkflowStore,
    lock: &LockedManagedWorkflow,
) -> (
    tempfile::TempDir,
    PreparedWorkflowRelease<'a>,
    ManagedWorkflowReceipt,
) {
    let (source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 60)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    (source_staging, prepared, receipt)
}

fn assert_committed(store: &ManagedWorkflowStore, receipt: &ManagedWorkflowReceipt) {
    assert_eq!(
        read_receipt(&store.receipts, &receipt.id).expect("committed receipt"),
        *receipt
    );
    let target = store.active_root.path().join(&receipt.id);
    assert!(target.join("src/workflow.ts").is_file());
    let active = SecureDirectory::open_root(&target).expect("active release");
    assert_eq!(
        read_marker(&active).expect("active marker").release,
        receipt.installed
    );
    assert!(!journal_exists(&store.journals, &receipt.id).expect("journal state"));
    assert!(
        store
            .staging
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("staging names")
            .is_empty()
    );
}

#[test]
fn fresh_install_commits_receipt_and_active_release() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, prepared, receipt) = prepared(&store, &lock);
    assert_eq!(
        store
            .commit_fresh(&lock, &ExpectedCurrent::Absent, prepared)
            .expect("commit"),
        ManagedWorkflowCommitOutcome::Committed
    );
    assert_committed(&store, &receipt);
    assert_eq!(
        store.recover_fresh(&lock).expect("idempotent recovery"),
        None
    );
}

#[test]
fn stale_receipt_and_unmanaged_target_fail_before_journal() {
    {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("workflow lock");
        let (_source, prepared, receipt) = prepared(&store, &lock);
        super::super::receipt::write_receipt(&store.receipts, &receipt, /*replace*/ false)
            .expect("concurrent receipt");
        assert!(
            store
                .commit_fresh(&lock, &ExpectedCurrent::Absent, prepared)
                .is_err()
        );
        assert!(!journal_exists(&store.journals, &receipt.id).expect("journal state"));
    }

    {
        let other = tempfile::tempdir().expect("other root");
        let store = store(other.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("workflow lock");
        let (_source, prepared, receipt) = prepared(&store, &lock);
        store
            .active_root
            .child("team")
            .expect("team directory")
            .child("build")
            .expect("unmanaged target");
        assert!(
            store
                .commit_fresh(&lock, &ExpectedCurrent::Absent, prepared)
                .is_err()
        );
        assert!(!journal_exists(&store.journals, &receipt.id).expect("journal state"));
    }
}

#[test]
fn recovery_rolls_forward_each_fresh_install_crash_point_twice() {
    for crash_after in ["journal", "publish", "receipt"] {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("workflow lock");
        let (_source, mut prepared, receipt) = prepared(&store, &lock);
        write_journal(&store.journals, &prepared.journal, /*replace*/ false)
            .expect("persist journal");
        prepared.staging.retain_for_recovery();
        if crash_after != "journal" {
            let parent = active_parent(&store.active_root, &receipt.id, ParentMode::Create)
                .expect("active parent")
                .expect("active parent exists");
            prepared
                .staging
                .directory()
                .rename_child_noreplace("payload", parent.directory(), "build")
                .expect("publish payload");
        }
        if crash_after == "receipt" {
            prepared.journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
            write_journal(&store.journals, &prepared.journal, /*replace*/ true)
                .expect("advance journal to receipt action");
            super::super::receipt::write_receipt(&store.receipts, &receipt, /*replace*/ false)
                .expect("write receipt");
        }
        drop(prepared);
        assert_eq!(
            store.recover_fresh(&lock).expect("first recovery"),
            Some(ManagedWorkflowCommitOutcome::Committed),
            "crash point {crash_after}"
        );
        assert_committed(&store, &receipt);
        assert_eq!(store.recover_fresh(&lock).expect("second recovery"), None);
    }
}

#[test]
fn missing_pending_payload_preserves_journal_and_fails_closed() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    fs::remove_dir_all(
        prepared
            .staging
            .directory()
            .path()
            .join("payload")
            .as_path(),
    )
    .expect("remove pending payload");
    drop(prepared);
    assert!(store.recover_fresh(&lock).is_err());
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
    assert!(!store.active_root.path().join(&receipt.id).exists());
}

#[test]
fn tampered_pending_payload_fails_before_publication() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let pending = prepared.staging.directory().path().join("payload");
    fs::write(pending.join("src/workflow.ts"), "tampered").expect("tamper pending payload");
    drop(prepared);
    assert!(store.recover_fresh(&lock).is_err());
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
    assert!(pending.is_dir());
    assert!(!store.active_root.path().join(&receipt.id).exists());
    assert!(
        read_pending_receipt(&store.receipts, &receipt.id)
            .expect("receipt state")
            .is_none()
    );
}

#[test]
fn lock_from_another_store_cannot_authorize_publication() {
    let first = tempfile::tempdir().expect("first root");
    let second = tempfile::tempdir().expect("second root");
    let first_store = store(first.path());
    let second_store = store(second.path());
    let first_lock = first_store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("first store lock");
    let (_source, first_prepared, receipt) = prepared(&first_store, &first_lock);
    assert!(
        second_store
            .commit_fresh(&first_lock, &ExpectedCurrent::Absent, first_prepared)
            .is_err()
    );
    assert!(second_store.recover_fresh(&first_lock).is_err());
    assert!(!journal_exists(&second_store.journals, &receipt.id).expect("no second journal"));

    let second_lock = second_store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("second store lock");
    let (_source, prepared, receipt) = prepared(&first_store, &first_lock);
    assert!(
        second_store
            .commit_fresh(&second_lock, &ExpectedCurrent::Absent, prepared)
            .is_err()
    );
    assert!(!journal_exists(&second_store.journals, &receipt.id).expect("no second journal"));
}

#[test]
fn recovery_rejects_actions_that_require_a_published_release() {
    for action in [
        ManagedWorkflowNextAction::WriteReceipt,
        ManagedWorkflowNextAction::Cleanup,
    ] {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("workflow lock");
        let (_source, mut prepared, receipt) = prepared(&store, &lock);
        prepared.journal.next_action = action;
        write_journal(&store.journals, &prepared.journal, /*replace*/ false)
            .expect("persist journal");
        prepared.staging.retain_for_recovery();
        drop(prepared);
        assert!(store.recover_fresh(&lock).is_err());
        assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
        assert!(!store.active_root.path().join(&receipt.id).exists());
    }
}

#[test]
fn recovery_rejects_tampered_published_payload_and_replaced_path() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &receipt.id, ParentMode::Create)
        .expect("active parent")
        .expect("active parent exists");
    prepared
        .staging
        .directory()
        .rename_child_noreplace("payload", parent.directory(), "build")
        .expect("publish payload");
    let published = parent
        .directory()
        .existing_child("build")
        .expect("published handle");
    let source_file = published.path().join("src/workflow.ts");
    let original = fs::read(source_file.as_path()).expect("original payload bytes");
    fs::write(source_file.as_path(), "tampered").expect("tamper active payload");
    drop(prepared);
    assert!(store.recover_fresh(&lock).is_err());
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
    assert!(
        read_pending_receipt(&store.receipts, &receipt.id)
            .expect("receipt state")
            .is_none()
    );

    fs::write(source_file.as_path(), original).expect("restore original payload");

    let moved = root.path().join("moved-active");
    fs::rename(published.path().as_path(), &moved).expect("move retained directory");
    parent
        .directory()
        .child("build")
        .expect("replacement target");
    let error = verify_published(
        &published,
        &read_journal(&store.journals, &receipt.id).expect("journal"),
    )
    .expect_err("replacement path must fail");
    assert!(error.to_string().contains("identity"), "{error:#}");
}

#[test]
fn committed_receipt_with_failed_cleanup_returns_pending_outcome() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &receipt.id, ParentMode::Create)
        .expect("active parent")
        .expect("active parent exists");
    prepared
        .staging
        .directory()
        .rename_child_noreplace("payload", parent.directory(), "build")
        .expect("publish payload");
    super::super::receipt::write_receipt(&store.receipts, &receipt, /*replace*/ false)
        .expect("commit receipt");
    prepared.journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
    write_journal(&store.journals, &prepared.journal, /*replace*/ true).expect("advance journal");
    let old_stage = root.path().join("moved-stage");
    fs::rename(prepared.staging.directory().path().as_path(), &old_stage)
        .expect("move owned staging root");
    store
        .staging
        .child(prepared.staging.name())
        .expect("replacement stage");
    drop(prepared);
    assert_eq!(
        store
            .recover_fresh(&lock)
            .expect("committed recovery outcome"),
        Some(ManagedWorkflowCommitOutcome::CommittedCleanupPending)
    );
    assert_eq!(
        read_receipt(&store.receipts, &receipt.id).expect("receipt"),
        receipt
    );
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
}

#[test]
fn recovery_preserves_conflicting_receipt_and_journal() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let mut conflicting = receipt.clone();
    conflicting.policy = super::super::receipt::WorkflowUpdatePolicy::Manual;
    super::super::receipt::write_receipt(&store.receipts, &conflicting, /*replace*/ false)
        .expect("conflicting receipt");
    drop(prepared);
    assert!(store.recover_fresh(&lock).is_err());
    assert_eq!(
        read_receipt(&store.receipts, &receipt.id).expect("receipt preserved"),
        conflicting
    );
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
}

#[test]
fn recovery_rejects_a_marker_for_another_transaction() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_source, mut prepared, receipt) = prepared(&store, &lock);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let mut marker = prepared.journal.marker();
    marker.transaction_id = "tx-different".into();
    prepared
        .staging
        .directory()
        .existing_child("payload")
        .expect("pending payload")
        .write_file(
            "codex-managed-workflow",
            &serde_json::to_vec(&marker).expect("marker JSON"),
            /*replace*/ true,
        )
        .expect("replace marker");
    drop(prepared);
    assert!(store.recover_fresh(&lock).is_err());
    assert!(journal_exists(&store.journals, &receipt.id).expect("journal remains"));
    assert!(!store.active_root.path().join(&receipt.id).exists());
}
