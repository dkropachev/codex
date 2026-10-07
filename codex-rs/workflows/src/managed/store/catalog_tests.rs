#![allow(clippy::expect_used)]

use std::path::Path;
use std::sync::atomic::AtomicBool;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::receipt::ManagedWorkflowReceipt;
use super::super::receipt::WorkflowRelease;
use super::super::receipt::WorkflowUpdatePolicy;
use super::super::receipt::write_receipt;
use super::*;

fn receipts(root: &Path) -> SecureDirectory {
    let root = AbsolutePathBuf::from_absolute_path_checked(root).expect("absolute root");
    SecureDirectory::open_root(&root)
        .expect("root")
        .child("receipts")
        .expect("receipts")
}

fn receipt(id: &str) -> ManagedWorkflowReceipt {
    ManagedWorkflowReceipt::new(
        id.into(),
        "https://example.com/team/workflow.git".into(),
        WorkflowRelease {
            tag: None,
            version: None,
            commit: "a".repeat(40),
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("receipt")
}

#[test]
fn catalog_is_sorted_and_enforces_both_limits() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    for id in ["team/z", "team/a", "other"] {
        write_receipt(&receipts, &receipt(id), /*replace*/ false).expect("write receipt");
    }
    let found = collect_receipts_with_limits(
        &receipts,
        CatalogLimits {
            receipts: 3,
            entries: 7,
        },
        /*cancelled*/ None,
    )
    .expect("exact catalog limits");
    assert_eq!(
        found
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["other", "team/a", "team/z"]
    );
    assert!(
        collect_receipts_with_limits(
            &receipts,
            CatalogLimits {
                receipts: 2,
                entries: 7
            },
            /*cancelled*/ None,
        )
        .is_err()
    );
    assert!(
        collect_receipts_with_limits(
            &receipts,
            CatalogLimits {
                receipts: 3,
                entries: 6
            },
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn catalog_rejects_unexpected_entries_aliases_and_cancellation() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    write_receipt(&receipts, &receipt("team/build"), /*replace*/ false).expect("write receipt");
    let cancelled = AtomicBool::new(true);
    assert!(collect_receipts(&receipts, Some(&cancelled)).is_err());
    std::fs::write(receipts.path().join("unexpected").as_path(), b"unexpected")
        .expect("unexpected file");
    assert!(collect_receipts(&receipts, /*cancelled*/ None).is_err());
    std::fs::remove_file(receipts.path().join("unexpected").as_path())
        .expect("remove unexpected file");
    std::os::unix::fs::symlink("team", receipts.path().join("alias").as_path())
        .expect("alias catalog directory");
    assert!(collect_receipts(&receipts, /*cancelled*/ None).is_err());
}

#[test]
fn catalog_rejects_orphan_id_directory() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    receipts.child("orphan").expect("orphan directory");
    let error = collect_receipts(&receipts, /*cancelled*/ None)
        .expect_err("reject orphan receipt directory");
    assert!(error.to_string().contains("orphan"), "{error:#}");
}
