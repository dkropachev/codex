#![allow(clippy::expect_used)]

use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn release() -> WorkflowRelease {
    WorkflowRelease {
        tag: Some("v1.2.3".into()),
        version: Some("1.2.3".into()),
        commit: "a".repeat(40),
    }
}

fn receipts(root: &Path) -> SecureDirectory {
    SecureDirectory::open_root(&absolute(root))
        .expect("open root")
        .child(".workflow-management")
        .expect("management")
        .child("receipts")
        .expect("receipts")
}

#[test]
fn receipt_v1_round_trip_is_exact_and_bounded() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    let mut receipt = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        release(),
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("receipt");
    write_receipt(&receipts, &receipt, /*replace*/ false).expect("write receipt");
    assert_eq!(
        read_receipt(&receipts, "team/build").expect("read receipt"),
        receipt
    );
    assert_eq!(
        serde_json::to_value(&receipt).expect("serialize initial receipt"),
        serde_json::json!({
            "schemaVersion": 1,
            "id": "team/build",
            "source": "https://example.com/team/build.git",
            "installed": {"tag": "v1.2.3", "version": "1.2.3", "commit": "a".repeat(40)},
            "policy": "prompt",
            "dismissedRelease": null
        })
    );
    assert!(write_receipt(&receipts, &receipt, /*replace*/ false).is_err());
    receipt.policy = WorkflowUpdatePolicy::Automatic;
    receipt.dismissed_release = Some(release());
    write_receipt(&receipts, &receipt, /*replace*/ true).expect("replace receipt");
    assert_eq!(
        read_receipt(&receipts, "team/build").expect("read replacement"),
        receipt
    );
    assert_eq!(
        serde_json::to_value(&receipt).expect("serialize replacement"),
        serde_json::json!({
            "schemaVersion": 1,
            "id": "team/build",
            "source": "https://example.com/team/build.git",
            "installed": {"tag": "v1.2.3", "version": "1.2.3", "commit": "a".repeat(40)},
            "policy": "automatic",
            "dismissedRelease": {"tag": "v1.2.3", "version": "1.2.3", "commit": "a".repeat(40)}
        })
    );
}

#[test]
fn oversized_receipt_fails_before_creating_id_directories() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    let version = format!("1.2.3+{}", "a".repeat(MAX_RECEIPT_BYTES));
    let receipt = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        WorkflowRelease {
            tag: Some(format!("v{version}")),
            version: Some(version),
            commit: "a".repeat(40),
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("valid but oversized receipt");
    assert!(write_receipt(&receipts, &receipt, /*replace*/ false).is_err());
    assert!(!receipts.path().join("team").exists());
}

#[test]
fn receipt_rejects_unknown_versions_fields_and_unsafe_values() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    let valid = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        release(),
        WorkflowUpdatePolicy::Manual,
    )
    .expect("valid receipt");
    for source in [
        "https://user:secret@example.com/repo",
        "file:///tmp/repo",
        "/tmp/repo",
        "C:\\repo",
    ] {
        let mut candidate = valid.clone();
        candidate.source = source.into();
        assert!(candidate.validate().is_err(), "{source}");
    }
    for id in [
        "team//build",
        "../build",
        "Team/build",
        "a".repeat(241).as_str(),
    ] {
        let mut candidate = valid.clone();
        candidate.id = id.into();
        assert!(candidate.validate().is_err(), "{id}");
    }
    let mut candidate = valid.clone();
    candidate.schema_version = 2;
    assert!(candidate.validate().is_err());
    let mut candidate = valid.clone();
    candidate.installed.commit = "invalid".into();
    assert!(candidate.validate().is_err());

    write_receipt(&receipts, &valid, /*replace*/ false).expect("write receipt");
    let directory = receipts
        .child("team")
        .expect("team")
        .child("build")
        .expect("build");
    directory
        .write_file(
            "receipt.json",
            b"{\"schemaVersion\":1,\"unknown\":true}",
            /*replace*/ true,
        )
        .expect("replace with unknown field");
    assert!(read_receipt(&receipts, "team/build").is_err());
}

#[test]
fn receipt_rejects_oversized_file_and_mismatched_directory_id() {
    let root = tempfile::tempdir().expect("root");
    let receipts = receipts(root.path());
    assert!(read_receipt(&receipts, "missing/workflow").is_err());
    assert!(!receipts.path().join("missing").exists());
    let receipt = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        release(),
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("valid receipt");
    write_receipt(&receipts, &receipt, /*replace*/ false).expect("write receipt");
    assert!(read_receipt(&receipts, "team/other").is_err());
    let directory = receipts
        .child("team")
        .expect("team")
        .child("build")
        .expect("build");
    directory
        .write_file(
            "receipt.json",
            &vec![b'X'; MAX_RECEIPT_BYTES + 1],
            /*replace*/ true,
        )
        .expect("write oversized fixture");
    assert!(read_receipt(&receipts, "team/build").is_err());
}
