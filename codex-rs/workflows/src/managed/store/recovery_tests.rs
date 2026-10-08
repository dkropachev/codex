#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::expected::ExpectedCurrent;
use super::super::journal::journal_file_name;
use super::super::journal::write_journal;
use super::super::prepare::tests::store;
use super::super::prepare::tests::verified_release;
use super::super::prepare::tests::verified_release_with_source;
use super::super::receipt::read_receipt;
use super::*;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn reopened(root: &Path) -> ManagedWorkflowStore {
    ManagedWorkflowStore::create(&absolute(root), &absolute(&root.join("workflows")))
        .expect("startup recovery")
}

#[test]
fn startup_rolls_forward_fresh_journal_twice() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let mut prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    write_journal(&store.journals, &prepared.journal, /*replace*/ false).expect("persist journal");
    prepared.staging.retain_for_recovery();
    drop(prepared);
    drop(lock);
    drop(store);

    let recovered = reopened(root.path());
    assert_eq!(
        read_receipt(&recovered.receipts, &receipt.id).expect("receipt"),
        receipt
    );
    assert!(
        recovered
            .active_root
            .path()
            .join("team/build/src/workflow.ts")
            .is_file()
    );
    assert!(
        recovered
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journal names")
            .is_empty()
    );
    drop(recovered);
    let again = reopened(root.path());
    assert!(
        again
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journal names")
            .is_empty()
    );
}

#[test]
fn startup_rolls_forward_replacement_journal() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let (_source, verified, previous, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            previous.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare first");
    assert_eq!(
        store
            .commit_fresh(&lock, &ExpectedCurrent::Absent, prepared)
            .expect("commit first"),
        ManagedWorkflowCommitOutcome::Committed
    );
    let (_source, verified, next, _) = verified_release_with_source(
        /*with_dependencies*/ false,
        /*synthetic_remote*/ true,
        "export default { updated: true };\n",
    );
    let mut prepared = store
        .prepare_release(
            &lock,
            verified,
            Some(previous),
            next.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare next");
    write_journal(&store.journals, &prepared.journal, /*replace*/ false)
        .expect("persist replacement journal");
    prepared.staging.retain_for_recovery();
    drop(prepared);
    drop(lock);
    drop(store);

    let recovered = reopened(root.path());
    assert_eq!(
        read_receipt(&recovered.receipts, &next.id).expect("next receipt"),
        next
    );
    assert_eq!(
        fs::read_to_string(
            recovered
                .active_root
                .path()
                .join("team/build/src/workflow.ts")
        )
        .expect("active source"),
        "export default { updated: true };\n"
    );
    assert!(
        recovered
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journal names")
            .is_empty()
    );
}

#[test]
fn startup_preserves_corrupt_journal_and_fails_closed() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let name = journal_file_name("team/build").expect("journal name");
    store
        .journals
        .write_file(&name, b"{broken", /*replace*/ false)
        .expect("corrupt journal fixture");
    drop(store);
    assert!(
        ManagedWorkflowStore::create(
            &absolute(root.path()),
            &absolute(&root.path().join("workflows")),
        )
        .is_err()
    );
    assert_eq!(
        fs::read(root.path().join(".workflow-management/journals").join(name))
            .expect("journal evidence preserved"),
        b"{broken"
    );
}

#[test]
fn startup_rejects_invalid_name_and_unknown_schema_without_removing_evidence() {
    let root = tempfile::tempdir().expect("root");
    let first_store = store(root.path());
    first_store
        .journals
        .write_file("unknown.json", b"{}", /*replace*/ false)
        .expect("invalid journal name");
    drop(first_store);
    assert!(
        ManagedWorkflowStore::create(
            &absolute(root.path()),
            &absolute(&root.path().join("workflows")),
        )
        .is_err()
    );
    assert!(
        root.path()
            .join(".workflow-management/journals/unknown.json")
            .is_file()
    );

    let second = tempfile::tempdir().expect("second root");
    let second_store = store(second.path());
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let mut journal = super::super::journal::ManagedWorkflowJournal::new(
        "tx-unknown-version".into(),
        receipt.id.clone(),
        /*previous_receipt*/ None,
        receipt.clone(),
        verified.evidence().clone(),
    )
    .expect("valid journal");
    journal.schema_version = 99;
    let name = journal_file_name(&receipt.id).expect("journal name");
    second_store
        .journals
        .write_file(
            &name,
            &serde_json::to_vec(&journal).expect("journal JSON"),
            /*replace*/ false,
        )
        .expect("unknown version journal");
    drop(second_store);
    assert!(
        ManagedWorkflowStore::create(
            &absolute(second.path()),
            &absolute(&second.path().join("workflows")),
        )
        .is_err()
    );
    assert!(
        second
            .path()
            .join(".workflow-management/journals")
            .join(name)
            .is_file()
    );
}

#[test]
fn startup_journal_scan_has_count_and_directory_bounds() {
    for (count, expected_fragment) in [(1_025, "count"), (4_097, "entry limit")] {
        let root = tempfile::tempdir().expect("root");
        let store = store(root.path());
        for index in 0..count {
            fs::write(
                store.journals.path().join(format!("orphan-{index:04}")),
                b"unknown",
            )
            .expect("journal directory fixture");
        }
        drop(store);
        let error = ManagedWorkflowStore::create(
            &absolute(root.path()),
            &absolute(&root.path().join("workflows")),
        )
        .err()
        .expect("bounded scan must fail");
        assert!(error.to_string().contains(expected_fragment), "{error:#}");
        assert!(
            root.path()
                .join(".workflow-management/journals/orphan-0000")
                .is_file()
        );
    }
}

#[test]
fn startup_recovery_waits_for_operations_and_observes_cancellation() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("install lock");
    let home = absolute(root.path());
    let workflows = absolute(&root.path().join("workflows"));
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        sender.send("started").expect("signal start");
        ManagedWorkflowStore::create(&home, &workflows).expect("recover after lock release");
        sender.send("completed").expect("signal completion");
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(/*secs*/ 1))
            .expect("start"),
        "started"
    );
    assert!(
        receiver
            .recv_timeout(Duration::from_millis(/*millis*/ 50))
            .is_err()
    );
    drop(lock);
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(/*secs*/ 2))
            .expect("recovery completion"),
        "completed"
    );
    worker.join().expect("recovery worker");
    assert!(super::recover_all(&store, Some(&AtomicBool::new(true))).is_err());
}

#[test]
fn startup_validates_every_journal_before_recovering_any() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock");
    let (_source, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let mut prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    write_journal(&store.journals, &prepared.journal, /*replace*/ false)
        .expect("persist valid journal");
    prepared.staging.retain_for_recovery();
    let valid_name = journal_file_name(&receipt.id).expect("valid journal name");
    let corrupt_name = (0..100)
        .map(|index| journal_file_name(&format!("team/bad-{index}")).expect("candidate name"))
        .find(|name| name > &valid_name)
        .expect("later-sorted journal name");
    store
        .journals
        .write_file(&corrupt_name, b"{broken", /*replace*/ false)
        .expect("persist corrupt journal");
    let pending = prepared.staging.directory().path().to_path_buf();
    drop(prepared);
    drop(lock);
    drop(store);

    assert!(
        ManagedWorkflowStore::create(
            &absolute(root.path()),
            &absolute(&root.path().join("workflows")),
        )
        .is_err()
    );
    assert!(!root.path().join("workflows/team/build").exists());
    assert!(pending.join("payload").is_dir());
    assert!(
        root.path()
            .join(".workflow-management/journals")
            .join(valid_name)
            .is_file()
    );
    assert!(
        root.path()
            .join(".workflow-management/journals")
            .join(corrupt_name)
            .is_file()
    );
}
