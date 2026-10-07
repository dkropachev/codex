#![allow(clippy::expect_used)]

use std::fs;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::super::expected::ReceiptIdentity;
use super::super::prepare::tests::store;
use super::super::prepare::tests::verified_release;
use super::super::prepare::tests::verified_release_with_source;
use super::super::receipt::read_receipt;
use super::*;

fn install_first(
    store: &ManagedWorkflowStore,
    lock: &LockedManagedWorkflow,
) -> super::super::receipt::ManagedWorkflowReceipt {
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare initial release");
    assert_eq!(
        store
            .commit_fresh(lock, &ExpectedCurrent::Absent, prepared)
            .expect("commit initial release"),
        ManagedWorkflowCommitOutcome::Committed
    );
    receipt
}

fn prepare_next<'a>(
    store: &'a ManagedWorkflowStore,
    lock: &LockedManagedWorkflow,
    previous: super::super::receipt::ManagedWorkflowReceipt,
) -> (
    tempfile::TempDir,
    PreparedWorkflowRelease<'a>,
    super::super::receipt::ManagedWorkflowReceipt,
) {
    let (source, verified, next, _) = verified_release_with_source(
        /*with_dependencies*/ false,
        /*synthetic_remote*/ true,
        "export default { updated: true };\n",
    );
    assert_ne!(previous.installed.commit, next.installed.commit);
    let prepared = store
        .prepare_release(
            lock,
            verified,
            Some(previous),
            next.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare replacement");
    (source, prepared, next)
}

fn assert_replaced(
    store: &ManagedWorkflowStore,
    next: &super::super::receipt::ManagedWorkflowReceipt,
) {
    assert_eq!(
        read_receipt(&store.receipts, &next.id).expect("next receipt"),
        *next
    );
    assert_eq!(
        fs::read_to_string(
            store
                .active_root
                .path()
                .join(&next.id)
                .join("src/workflow.ts")
        )
        .expect("active source"),
        "export default { updated: true };\n"
    );
    assert!(
        store
            .backups
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("backup names")
            .is_empty()
    );
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("journal state"));
}

#[test]
fn replacement_commits_new_release_and_removes_backup() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let expected = ExpectedCurrent::Receipt(
        ReceiptIdentity::from_receipt(&previous).expect("previous identity"),
    );
    let (_source, prepared, next) = prepare_next(&store, &lock, previous);
    assert_eq!(
        store
            .commit_replace(&lock, &expected, prepared)
            .expect("commit replacement"),
        ManagedWorkflowCommitOutcome::Committed
    );
    assert_replaced(&store, &next);
    assert_eq!(store.recover_replace(&lock).expect("second recovery"), None);
}

#[test]
fn missing_pending_payload_restores_verified_previous_release() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
        .expect("parent")
        .expect("existing parent");
    backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
        .expect("move old release aside");
    fs::remove_dir_all(
        prepared
            .staging
            .directory()
            .path()
            .join("payload")
            .as_path(),
    )
    .expect("lose pending payload");
    drop(prepared);
    assert_eq!(
        store.recover_replace(&lock).expect("rollback"),
        Some(ManagedWorkflowCommitOutcome::RolledBack)
    );
    assert_eq!(
        read_receipt(&store.receipts, &previous.id).expect("previous receipt"),
        previous
    );
    assert_eq!(
        fs::read_to_string(
            store
                .active_root
                .path()
                .join(&next.id)
                .join("src/workflow.ts")
        )
        .expect("restored source"),
        "export default {};\n"
    );
    assert!(
        store
            .backups
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("backup names")
            .is_empty()
    );
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("journal gone"));
    assert_eq!(store.recover_replace(&lock).expect("second recovery"), None);
}

#[test]
fn recovery_rolls_forward_each_replacement_crash_point_twice() {
    for crash_after in ["journal", "backup", "publish", "receipt"] {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("lock");
        let previous = install_first(&store, &lock);
        let (_source, mut prepared, next) = prepare_next(&store, &lock, previous);
        write_journal(&store.journals, &prepared.journal, /*replace*/ false)
            .expect("persist replacement journal");
        prepared.staging.retain_for_recovery();
        let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
            .expect("active parent")
            .expect("active parent exists");
        if crash_after != "journal" {
            backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
                .expect("move old release aside");
        }
        if matches!(crash_after, "publish" | "receipt") {
            prepared.journal.next_action = ManagedWorkflowNextAction::PublishRelease;
            write_journal(&store.journals, &prepared.journal, /*replace*/ true)
                .expect("advance to publish action");
            prepared
                .staging
                .directory()
                .rename_child_noreplace("payload", parent.directory(), "build")
                .expect("publish replacement");
        }
        if crash_after == "receipt" {
            prepared.journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
            write_journal(&store.journals, &prepared.journal, /*replace*/ true)
                .expect("advance to receipt action");
            write_receipt(&store.receipts, &next, /*replace*/ true).expect("commit next receipt");
        }
        drop(prepared);
        assert_eq!(
            store.recover_replace(&lock).expect("recover replacement"),
            Some(ManagedWorkflowCommitOutcome::Committed),
            "crash point {crash_after}"
        );
        assert_replaced(&store, &next);
        assert_eq!(
            store.recover_replace(&lock).expect("idempotent recovery"),
            None
        );
    }
}

#[test]
fn recovery_finishes_rollback_after_backup_was_restored() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
        .expect("parent")
        .expect("existing parent");
    backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
        .expect("move old release aside");
    prepared.journal.next_action = ManagedWorkflowNextAction::PublishRelease;
    write_journal(&store.journals, &prepared.journal, /*replace*/ true).expect("advance journal");
    fs::remove_dir_all(
        prepared
            .staging
            .directory()
            .path()
            .join("payload")
            .as_path(),
    )
    .expect("lose pending payload");
    backup::restore_previous(&store, &prepared.journal, parent.directory(), "build")
        .expect("restore backup before crash");
    drop(prepared);
    assert_eq!(
        store.recover_replace(&lock).expect("finish rollback"),
        Some(ManagedWorkflowCommitOutcome::RolledBack)
    );
    assert_eq!(
        read_receipt(&store.receipts, &previous.id).expect("receipt"),
        previous
    );
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("journal gone"));
    assert_eq!(store.recover_replace(&lock).expect("second recovery"), None);
}

#[test]
fn tampered_previous_release_or_backup_cannot_be_used_for_replacement() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let expected = ExpectedCurrent::Receipt(
        ReceiptIdentity::from_receipt(&previous).expect("previous identity"),
    );
    let active_source = store
        .active_root
        .path()
        .join(&previous.id)
        .join("src/workflow.ts");
    fs::write(active_source.as_path(), "tampered old source").expect("tamper old active payload");
    let (_source, prepared, next) = prepare_next(&store, &lock, previous.clone());
    assert!(store.commit_replace(&lock, &expected, prepared).is_err());
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("no journal"));

    fs::write(active_source.as_path(), "export default {};\n").expect("restore old payload");
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
        .expect("parent")
        .expect("existing parent");
    backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
        .expect("move old release aside");
    let backup_source = store
        .backups
        .path()
        .join(&prepared.journal.transaction_id)
        .join("src/workflow.ts");
    fs::write(backup_source.as_path(), "tampered backup").expect("tamper backup payload");
    fs::remove_dir_all(
        prepared
            .staging
            .directory()
            .path()
            .join("payload")
            .as_path(),
    )
    .expect("lose pending payload");
    drop(prepared);
    assert!(store.recover_replace(&lock).is_err());
    assert!(publish::journal_exists(&store.journals, &next.id).expect("journal preserved"));
    assert!(!store.active_root.path().join(&next.id).exists());
    assert_eq!(
        read_receipt(&store.receipts, &previous.id).expect("receipt"),
        previous
    );
}

#[test]
fn stale_expected_receipt_and_unmanaged_target_fail_before_journal() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, prepared, next) = prepare_next(&store, &lock, previous.clone());
    assert!(
        store
            .commit_replace(&lock, &ExpectedCurrent::Absent, prepared)
            .is_err()
    );
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("no journal"));

    let active = store.active_root.path().join(&previous.id);
    fs::remove_dir_all(active.as_path()).expect("remove old active fixture");
    fs::create_dir(active.as_path()).expect("unmanaged target");
    let (_source, prepared, next) = prepare_next(&store, &lock, previous.clone());
    let expected = ExpectedCurrent::Receipt(
        ReceiptIdentity::from_receipt(&previous).expect("previous identity"),
    );
    assert!(store.commit_replace(&lock, &expected, prepared).is_err());
    assert!(active.is_dir());
    assert!(!publish::journal_exists(&store.journals, &next.id).expect("no journal"));
}

#[test]
fn recovery_rejects_conflicting_receipt() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let mut conflicting = previous;
    conflicting.policy = super::super::receipt::WorkflowUpdatePolicy::Manual;
    write_receipt(&store.receipts, &conflicting, /*replace*/ true)
        .expect("write conflicting receipt");
    drop(prepared);
    assert!(store.recover_replace(&lock).is_err());
    assert_eq!(
        read_receipt(&store.receipts, &next.id).expect("receipt"),
        conflicting
    );
    assert!(publish::journal_exists(&store.journals, &next.id).expect("journal remains"));
}

#[test]
fn recovery_rejects_changed_pending_payload_or_marker() {
    for change in ["payload", "marker"] {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        let lock = store
            .lock_install("team/build", /*cancelled*/ None)
            .expect("lock");
        let previous = install_first(&store, &lock);
        let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
        write_journal(&store.journals, &prepared.journal, /*replace*/ false)
            .expect("persist journal");
        prepared.staging.retain_for_recovery();
        let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
            .expect("parent")
            .expect("existing parent");
        backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
            .expect("move old release aside");
        let payload = prepared.staging.directory().path().join("payload");
        if change == "payload" {
            fs::write(payload.join("src/workflow.ts"), "tampered pending source")
                .expect("tamper pending payload");
        } else {
            let mut marker = prepared.journal.marker();
            marker.transaction_id = "tx-other".into();
            fs::write(
                payload.join("codex-managed-workflow"),
                serde_json::to_vec(&marker).expect("marker JSON"),
            )
            .expect("tamper pending marker");
        }
        drop(prepared);
        assert!(store.recover_replace(&lock).is_err(), "change {change}");
        assert!(publish::journal_exists(&store.journals, &next.id).expect("journal remains"));
        assert!(
            store
                .backups
                .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
                .expect("backup names")
                .iter()
                .any(|name| name.starts_with("tx-"))
        );
        assert!(!store.active_root.path().join(&next.id).exists());
        assert_eq!(
            read_receipt(&store.receipts, &next.id).expect("previous receipt"),
            previous
        );
    }
}

#[test]
fn recovery_rejects_old_active_release_beside_its_backup() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous.clone());
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
        .expect("parent")
        .expect("existing parent");
    backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
        .expect("move old release aside");
    let backup_path = store.backups.path().join(&prepared.journal.transaction_id);
    let active_path = parent.directory().path().join("build");
    fs::create_dir(active_path.as_path()).expect("duplicate old active");
    fs::create_dir(active_path.join("src").as_path()).expect("duplicate source directory");
    for file in [
        "codex-managed-workflow",
        "workflow.yaml",
        "package.json",
        "src/workflow.ts",
    ] {
        fs::copy(
            backup_path.join(file).as_path(),
            active_path.join(file).as_path(),
        )
        .expect("duplicate old payload file");
    }
    drop(prepared);
    assert!(store.recover_replace(&lock).is_err());
    assert!(active_path.is_dir() && backup_path.is_dir());
    assert!(publish::journal_exists(&store.journals, &next.id).expect("journal remains"));
    assert_eq!(
        read_receipt(&store.receipts, &next.id).expect("receipt"),
        previous
    );
}

#[test]
fn committed_replacement_with_failed_cleanup_reports_pending() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let previous = install_first(&store, &lock);
    let (_source, mut prepared, next) = prepare_next(&store, &lock, previous);
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    let parent = active_parent(&store.active_root, &next.id, ParentMode::Existing)
        .expect("parent")
        .expect("existing parent");
    backup::move_current_aside(&store, &prepared.journal, parent.directory(), "build")
        .expect("move old release aside");
    prepared.journal.next_action = ManagedWorkflowNextAction::PublishRelease;
    write_journal(&store.journals, &prepared.journal, /*replace*/ true)
        .expect("advance to publish");
    prepared
        .staging
        .directory()
        .rename_child_noreplace("payload", parent.directory(), "build")
        .expect("publish replacement");
    prepared.journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
    write_journal(&store.journals, &prepared.journal, /*replace*/ true)
        .expect("advance to receipt");
    write_receipt(&store.receipts, &next, /*replace*/ true).expect("commit receipt");
    let moved_stage = root.path().join("moved-staging");
    fs::rename(prepared.staging.directory().path().as_path(), &moved_stage)
        .expect("move owned staging root");
    store
        .staging
        .child(prepared.staging.name())
        .expect("replacement staging root");
    drop(prepared);
    assert_eq!(
        store
            .recover_replace(&lock)
            .expect("committed recovery outcome"),
        Some(ManagedWorkflowCommitOutcome::CommittedCleanupPending)
    );
    assert_eq!(
        read_receipt(&store.receipts, &next.id).expect("committed receipt"),
        next
    );
    assert!(publish::journal_exists(&store.journals, &next.id).expect("journal remains"));
}
