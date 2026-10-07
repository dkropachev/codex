use std::fs;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::super::expected::ExpectedCurrent;
use super::super::journal::ManagedWorkflowJournal;
use super::super::prepare::tests::store;
use super::super::prepare::tests::verified_release;
use super::super::publish::ManagedWorkflowCommitOutcome;
use super::super::receipt::ManagedWorkflowReceipt;
use super::*;

fn installed(
    store: &ManagedWorkflowStore,
    lock: &super::super::LockedManagedWorkflow,
) -> (ManagedWorkflowReceipt, ManagedWorkflowJournal) {
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare old release");
    let evidence = prepared.journal.evidence.clone();
    assert_eq!(
        store
            .commit_fresh(lock, &ExpectedCurrent::Absent, prepared)
            .expect("install old release"),
        ManagedWorkflowCommitOutcome::Committed
    );
    let journal = ManagedWorkflowJournal::new(
        "tx-backup-fixture".into(),
        receipt.id.clone(),
        Some(receipt.clone()),
        receipt.clone(),
        evidence,
    )
    .expect("replacement journal");
    (receipt, journal)
}

#[test]
fn marked_backup_moves_and_restores_the_same_verified_release() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (receipt, journal) = installed(&store, &lock);
    let parent = store
        .active_root
        .existing_child("team")
        .expect("active parent");
    move_current_aside(&store, &journal, &parent, "build").expect("move aside");
    move_current_aside(&store, &journal, &parent, "build").expect("idempotent binding");
    assert!(!parent.path().join("build").exists());
    assert!(store.backups.path().join(&journal.transaction_id).is_dir());
    restore_previous(&store, &journal, &parent, "build").expect("restore backup");
    assert_eq!(
        read_marker(&parent.existing_child("build").expect("restored active"))
            .expect("manager marker")
            .release,
        receipt.installed
    );
    assert!(
        store
            .backups
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("backup names")
            .is_empty()
    );
}

#[test]
fn changed_old_payload_cannot_move_or_restore() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let (_receipt, journal) = installed(&store, &lock);
    let parent = store
        .active_root
        .existing_child("team")
        .expect("active parent");
    let old_source = parent.path().join("build/src/workflow.ts");
    fs::write(old_source.as_path(), "tampered").expect("tamper active source");
    assert!(move_current_aside(&store, &journal, &parent, "build").is_err());
    assert!(parent.path().join("build").is_dir());
    fs::write(old_source.as_path(), "export default {};\n").expect("restore old source");
    move_current_aside(&store, &journal, &parent, "build").expect("move old release");
    let backup_source = store
        .backups
        .path()
        .join(&journal.transaction_id)
        .join("src/workflow.ts");
    fs::write(backup_source.as_path(), "tampered backup").expect("tamper backup");
    assert!(restore_previous(&store, &journal, &parent, "build").is_err());
    assert!(!parent.path().join("build").exists());
    assert!(store.backups.path().join(&journal.transaction_id).is_dir());
}
