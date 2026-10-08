#![allow(clippy::expect_used)]

use std::fs;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::expected::ReceiptIdentity;
use super::super::prepare::tests::store;
use super::super::prepare::tests::verified_release;
use super::super::receipt::ManagedWorkflowReceipt;
use super::*;

fn installed(store: &ManagedWorkflowStore) -> (LockedManagedWorkflow, ManagedWorkflowReceipt) {
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    assert_eq!(
        store
            .commit_fresh(&lock, &ExpectedCurrent::Absent, prepared)
            .expect("install release"),
        ManagedWorkflowCommitOutcome::Committed
    );
    (lock, receipt)
}

fn expected(receipt: &ManagedWorkflowReceipt) -> ExpectedCurrent {
    ExpectedCurrent::Receipt(ReceiptIdentity::from_receipt(receipt).expect("receipt identity"))
}

#[test]
fn uninstall_removes_managed_payload_and_receipt() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let (lock, receipt) = installed(&store);
    assert_eq!(
        store
            .commit_uninstall(&lock, &expected(&receipt))
            .expect("uninstall release"),
        ManagedWorkflowCommitOutcome::Committed
    );
    drop(lock);
    assert!(!root.path().join("workflows/team/build").exists());
    assert_eq!(
        store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog"),
        Vec::new()
    );
    assert!(
        store
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journals")
            .is_empty()
    );
    assert!(
        store
            .backups
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("backups")
            .is_empty()
    );
    assert!(
        store
            .staging
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("staging")
            .is_empty()
    );
}

#[test]
fn uninstall_rejects_dirty_payload_and_stale_receipt() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let (lock, receipt) = installed(&store);
    assert!(
        store
            .commit_uninstall(&lock, &ExpectedCurrent::Absent)
            .is_err()
    );
    let source = root.path().join("workflows/team/build/src/workflow.ts");
    fs::write(&source, "tampered").expect("tamper active source");
    assert!(store.commit_uninstall(&lock, &expected(&receipt)).is_err());
    assert!(root.path().join("workflows/team/build").is_dir());
    drop(lock);
    assert_eq!(
        store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog"),
        vec![receipt]
    );
    assert!(
        store
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journals")
            .is_empty()
    );
}

#[test]
fn startup_recovers_a_journal_before_moving_the_release() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let (lock, receipt) = installed(&store);
    let active = store
        .active_root
        .existing_child("team")
        .expect("parent")
        .existing_child("build")
        .expect("active release");
    let evidence = published_payload_evidence(
        active.path().as_path(),
        VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect("published evidence");
    drop(active);
    let mut staging = TransactionStaging::create(&store.staging).expect("staging");
    let journal =
        ManagedWorkflowJournal::new_uninstall(staging.name().to_owned(), receipt, evidence)
            .expect("uninstall journal");
    write_journal(&store.journals, &journal, /*replace*/ false).expect("persist journal");
    staging.retain_for_recovery();
    drop(staging);
    drop(lock);
    drop(store);
    let home = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("home");
    let workflow_root = AbsolutePathBuf::from_absolute_path_checked(root.path().join("workflows"))
        .expect("workflow root");
    let recovered =
        ManagedWorkflowStore::create(&home, &workflow_root).expect("startup uninstall recovery");
    assert!(!root.path().join("workflows/team/build").exists());
    assert_eq!(
        recovered
            .list_receipts(/*cancelled*/ None)
            .expect("catalog"),
        Vec::new()
    );
}
