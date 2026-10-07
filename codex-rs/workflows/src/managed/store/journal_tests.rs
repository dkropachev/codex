use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::receipt::WorkflowUpdatePolicy;
use super::*;

fn directory(root: &Path, name: &str) -> SecureDirectory {
    let root = AbsolutePathBuf::from_absolute_path_checked(root).expect("absolute root");
    SecureDirectory::open_root(&root)
        .expect("open root")
        .child(name)
        .expect("private directory")
}

fn receipt(id: &str) -> ManagedWorkflowReceipt {
    ManagedWorkflowReceipt::new(
        id.into(),
        "https://example.com/team/workflow.git".into(),
        WorkflowRelease {
            tag: Some("v1.2.3".into()),
            version: Some("1.2.3".into()),
            commit: "a".repeat(40),
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("receipt")
}

fn evidence() -> ActivationPayloadEvidence {
    ActivationPayloadEvidence {
        format_version: 1,
        sha256: "b".repeat(64),
        entry_count: 10,
        logical_bytes: 100,
    }
}

#[test]
fn journal_and_marker_round_trip_with_exact_v1_fields() {
    let root = tempfile::tempdir().expect("root");
    let journals = directory(root.path(), "journals");
    let target = directory(root.path(), "target");
    let journal = ManagedWorkflowJournal::new(
        "tx-123".into(),
        "team/build".into(),
        Some(receipt("team/build")),
        receipt("team/build"),
        evidence(),
    )
    .expect("journal");
    assert_eq!(journal.operation, ManagedWorkflowOperation::Replace);
    assert_eq!(
        journal.next_action,
        ManagedWorkflowNextAction::MoveCurrentAside
    );
    write_journal(&journals, &journal, /*replace*/ false).expect("write journal");
    assert_eq!(
        read_journal(&journals, "team/build").expect("read journal"),
        journal
    );
    let marker = journal.marker();
    write_marker(&target, &marker).expect("write marker");
    assert_eq!(read_marker(&target).expect("read marker"), marker);
    assert!(marker.matches_journal(&journal));
    assert_eq!(
        serde_json::to_value(&marker).expect("serialize marker"),
        serde_json::json!({
            "schemaVersion": 1,
            "id": "team/build",
            "transactionId": "tx-123",
            "release": {"tag": "v1.2.3", "version": "1.2.3", "commit": "a".repeat(40)},
            "evidenceDigest": "b".repeat(64),
        })
    );
    assert_eq!(
        serde_json::to_value(&journal).expect("serialize journal"),
        serde_json::json!({
            "schemaVersion": 1,
            "transactionId": "tx-123",
            "id": "team/build",
            "operation": "replace",
            "previousReceipt": serde_json::to_value(receipt("team/build")).expect("previous"),
            "nextReceipt": serde_json::to_value(receipt("team/build")).expect("next"),
            "evidence": {"formatVersion": 1, "sha256": "b".repeat(64), "entryCount": 10, "logicalBytes": 100},
            "nextAction": "moveCurrentAside",
        })
    );
}

#[test]
fn journal_rejects_corruption_unknown_versions_and_mismatched_state() {
    let root = tempfile::tempdir().expect("root");
    let journals = directory(root.path(), "journals");
    let target = directory(root.path(), "target");
    let valid = ManagedWorkflowJournal::new(
        "tx-123".into(),
        "team/build".into(),
        None,
        receipt("team/build"),
        evidence(),
    )
    .expect("fresh journal");
    assert_eq!(valid.next_action, ManagedWorkflowNextAction::PublishRelease);
    for corrupt in [
        ("version", {
            let mut value = valid.clone();
            value.schema_version = 2;
            value
        }),
        ("id", {
            let mut value = valid.clone();
            value.id = "team/other".into();
            value
        }),
        ("transaction", {
            let mut value = valid.clone();
            value.transaction_id = "../escape".into();
            value
        }),
        ("digest", {
            let mut value = valid.clone();
            value.evidence.sha256 = "bad".into();
            value
        }),
        ("operation", {
            let mut value = valid.clone();
            value.operation = ManagedWorkflowOperation::Replace;
            value
        }),
    ] {
        assert!(corrupt.1.validate().is_err(), "{}", corrupt.0);
    }
    write_journal(&journals, &valid, /*replace*/ false).expect("write journal");
    let name = journal_file_name("team/build").expect("journal file name");
    journals
        .write_file(&name, b"{not-json", /*replace*/ true)
        .expect("corrupt journal");
    assert!(read_journal(&journals, "team/build").is_err());
    let marker = valid.marker();
    let mut mismatch = marker.clone();
    mismatch.transaction_id = "tx-456".into();
    assert!(!mismatch.matches_journal(&valid));
    write_marker(&target, &marker).expect("write marker");
    let mut version = marker;
    version.schema_version = 2;
    assert!(version.validate().is_err());
}

#[test]
fn journal_name_is_bounded_and_collision_resistant_for_distinct_ids() {
    let first = journal_file_name("team/a").expect("first id");
    let second = journal_file_name("team_a").expect("second id");
    assert_ne!(first, second);
    assert_eq!(first.len(), 69);
    assert!(first.ends_with(".json"));
}

#[test]
fn journal_and_marker_caps_fail_before_publication_and_on_oversized_reads() {
    let root = tempfile::tempdir().expect("root");
    let journals = directory(root.path(), "journals");
    let target = directory(root.path(), "target");
    let valid = ManagedWorkflowJournal::new(
        "tx-123".into(),
        "team/build".into(),
        None,
        receipt("team/build"),
        evidence(),
    )
    .expect("valid journal");
    let mut oversized_receipt = valid.clone();
    let version = format!("1.2.3+{}", "a".repeat(70 * 1024));
    oversized_receipt.next_receipt.installed.version = Some(version.clone());
    oversized_receipt.next_receipt.installed.tag = Some(format!("v{version}"));
    assert!(write_journal(&journals, &oversized_receipt, /*replace*/ false).is_err());
    assert!(
        !journals
            .path()
            .join(journal_file_name("team/build").expect("name"))
            .exists()
    );

    let mut oversized_marker = valid.marker();
    let version = format!("1.2.3+{}", "a".repeat(MAX_MARKER_BYTES));
    oversized_marker.release.version = Some(version.clone());
    oversized_marker.release.tag = Some(format!("v{version}"));
    assert!(write_marker(&target, &oversized_marker).is_err());
    assert!(!target.path().join("codex-managed-workflow").exists());

    let near_boundary = (MAX_MARKER_BYTES / 2 - 400..MAX_MARKER_BYTES / 2 + 100)
        .find_map(|count| {
            let mut candidate = valid.clone();
            let version = format!("1.2.3+{}", "a".repeat(count));
            candidate.next_receipt.installed.version = Some(version.clone());
            candidate.next_receipt.installed.tag = Some(format!("v{version}"));
            let marker = candidate.marker();
            let compact = serde_json::to_vec(&marker).expect("compact marker");
            let persisted = serialize_marker(&marker).expect("persisted marker");
            (compact.len() <= MAX_MARKER_BYTES && persisted.len() > MAX_MARKER_BYTES)
                .then_some(candidate)
        })
        .expect("fixture crosses compact/pretty size boundary");
    assert!(near_boundary.validate().is_err());

    let journal_name = journal_file_name("team/build").expect("name");
    journals
        .write_file(
            &journal_name,
            &vec![b'X'; MAX_JOURNAL_BYTES + 1],
            /*replace*/ false,
        )
        .expect("oversized on-disk journal fixture");
    assert!(read_journal(&journals, "team/build").is_err());
    target
        .write_file(
            "codex-managed-workflow",
            &vec![b'X'; MAX_MARKER_BYTES + 1],
            /*replace*/ false,
        )
        .expect("oversized on-disk marker fixture");
    assert!(read_marker(&target).is_err());
}
