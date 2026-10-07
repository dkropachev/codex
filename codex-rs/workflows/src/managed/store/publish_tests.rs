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
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
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
