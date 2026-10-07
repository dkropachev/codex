use std::fs;
use std::path::Path;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::expected::ReceiptIdentity;
use super::receipt::WorkflowRelease;
use super::receipt::WorkflowUpdatePolicy;
use super::*;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn store(root: &Path) -> ManagedWorkflowStore {
    fs::create_dir(root.join("workflows")).expect("workflow root");
    ManagedWorkflowStore::create(&absolute(root), &absolute(&root.join("workflows")))
        .expect("managed store")
}

fn prepared<'a>(
    store: &'a ManagedWorkflowStore,
    commit: char,
    contents: &str,
    previous: Option<ManagedWorkflowReceipt>,
) -> PreparedWorkflowRelease<'a> {
    let staging = stage::TransactionStaging::create(&store.staging).expect("transaction staging");
    let payload = staging
        .directory()
        .child("payload")
        .expect("payload directory");
    payload
        .write_file("workflow.yaml", b"workflow", /*replace*/ false)
        .expect("manifest");
    payload
        .child("src")
        .expect("source directory")
        .write_file("workflow.ts", contents.as_bytes(), /*replace*/ false)
        .expect("source file");
    let evidence = crate::managed::integrity::published_payload_evidence(
        payload.path().as_path(),
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect("payload evidence");
    let next = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        WorkflowRelease {
            tag: None,
            version: None,
            commit: commit.to_string().repeat(40),
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("receipt");
    let journal = journal::ManagedWorkflowJournal::new(
        staging.name().to_owned(),
        next.id.clone(),
        previous,
        next,
        evidence,
    )
    .expect("journal");
    journal::write_marker(&payload, &journal.marker()).expect("manager marker");
    PreparedWorkflowRelease { staging, journal }
}

#[test]
fn windows_install_and_replace_commit_matching_receipts() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let first = prepared(&store, 'a', "export default {};", /*previous*/ None);
    let first_receipt = first.journal.next_receipt.clone();
    assert_eq!(
        store
            .commit_fresh(&lock, &ExpectedCurrent::Absent, first)
            .expect("install"),
        ManagedWorkflowCommitOutcome::Committed
    );
    assert_eq!(
        receipt::read_receipt(&store.receipts, "team/build").expect("first receipt"),
        first_receipt
    );

    let next = prepared(
        &store,
        'b',
        "export default { updated: true };",
        Some(first_receipt.clone()),
    );
    let next_receipt = next.journal.next_receipt.clone();
    let expected = ExpectedCurrent::Receipt(
        ReceiptIdentity::from_receipt(&first_receipt).expect("old identity"),
    );
    assert_eq!(
        store
            .commit_replace(&lock, &expected, next)
            .expect("replacement"),
        ManagedWorkflowCommitOutcome::Committed
    );
    assert_eq!(
        receipt::read_receipt(&store.receipts, "team/build").expect("next receipt"),
        next_receipt
    );
    assert_eq!(
        fs::read_to_string(store.active_root.path().join("team/build/src/workflow.ts"))
            .expect("active source"),
        "export default { updated: true };"
    );
}

#[test]
fn windows_startup_rolls_forward_pending_install_twice() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let mut pending = prepared(&store, 'c', "export default {};", /*previous*/ None);
    let receipt = pending.journal.next_receipt.clone();
    journal::write_journal(&store.journals, &pending.journal, /*replace*/ false)
        .expect("persist journal");
    pending.staging.retain_for_recovery();
    drop(pending);
    drop(lock);
    drop(store);

    let recovered = ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("startup recovery");
    assert_eq!(
        receipt::read_receipt(&recovered.receipts, "team/build").expect("receipt"),
        receipt
    );
    assert!(
        recovered
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journal names")
            .is_empty()
    );
    drop(recovered);
    ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("idempotent recovery");
}

#[test]
fn windows_startup_rolls_forward_replacement_after_backup_move() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let first = prepared(&store, 'a', "export default {};", /*previous*/ None);
    let previous = first.journal.next_receipt.clone();
    store
        .commit_fresh(&lock, &ExpectedCurrent::Absent, first)
        .expect("install first");
    let mut next = prepared(
        &store,
        'b',
        "export default { updated: true };",
        Some(previous),
    );
    let next_receipt = next.journal.next_receipt.clone();
    journal::write_journal(&store.journals, &next.journal, /*replace*/ false)
        .expect("persist replacement journal");
    next.staging.retain_for_recovery();
    let parent = store
        .active_root
        .existing_child("team")
        .expect("active parent");
    backup::move_current_aside(&store, &next.journal, &parent, "build")
        .expect("move old release aside");
    drop(parent);
    drop(next);
    drop(lock);
    drop(store);

    let recovered = ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("replacement startup recovery");
    assert_eq!(
        receipt::read_receipt(&recovered.receipts, "team/build").expect("next receipt"),
        next_receipt
    );
    assert!(
        recovered
            .journals
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("journal names")
            .is_empty()
    );
    drop(recovered);
    ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("idempotent replacement recovery");
}

#[test]
fn windows_startup_preserves_corrupt_journal_and_marker() {
    let root = tempfile::tempdir().expect("root");
    let first_store = store(root.path());
    let journal_name = journal::journal_file_name("team/build").expect("journal name");
    first_store
        .journals
        .write_file(&journal_name, b"{broken", /*replace*/ false)
        .expect("corrupt journal");
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
            .join(".workflow-management/journals")
            .join(journal_name)
            .is_file()
    );

    let second = tempfile::tempdir().expect("second root");
    let second_store = store(second.path());
    let lock = second_store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("workflow lock");
    let mut pending = prepared(
        &second_store,
        'c',
        "export default {};",
        /*previous*/ None,
    );
    journal::write_journal(
        &second_store.journals,
        &pending.journal,
        /*replace*/ false,
    )
    .expect("persist journal");
    pending.staging.retain_for_recovery();
    let payload = pending
        .staging
        .directory()
        .existing_child("payload")
        .expect("pending payload");
    let mut marker = pending.journal.marker();
    marker.transaction_id = "tx-different".into();
    payload
        .write_file(
            "codex-managed-workflow",
            &serde_json::to_vec(&marker).expect("marker JSON"),
            /*replace*/ true,
        )
        .expect("mismatched marker");
    let pending_path = pending.staging.directory().path().to_path_buf();
    drop(payload);
    drop(pending);
    drop(lock);
    drop(second_store);
    assert!(
        ManagedWorkflowStore::create(
            &absolute(second.path()),
            &absolute(&second.path().join("workflows")),
        )
        .is_err()
    );
    assert!(pending_path.join("payload").is_dir());
    assert!(!second.path().join("workflows/team/build").exists());
}
