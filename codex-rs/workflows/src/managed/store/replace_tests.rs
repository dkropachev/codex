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
