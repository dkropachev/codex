use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::receipt::WorkflowRelease;
use super::super::receipt::WorkflowUpdatePolicy;
use super::super::receipt::write_receipt;
use super::*;

#[test]
fn compare_current_requires_the_exact_observed_receipt() {
    let root = tempfile::tempdir().expect("root");
    let absolute = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("root path");
    let receipts = SecureDirectory::open_root(&absolute)
        .expect("open root")
        .child("receipts")
        .expect("receipts directory");
    assert_eq!(
        compare_current(&receipts, "team/build", &ExpectedCurrent::Absent).expect("absent receipt"),
        None
    );
    receipts
        .child("team")
        .expect("team receipts")
        .child("orphan")
        .expect("orphan receipt directory");
    assert!(compare_current(&receipts, "team/orphan", &ExpectedCurrent::Absent).is_err());
    assert_eq!(
        super::super::receipt::read_pending_receipt(&receipts, "team/orphan")
            .expect("pending transaction can observe empty receipt leaf"),
        None
    );

    let first = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        WorkflowRelease {
            tag: None,
            version: None,
            commit: "a".repeat(40),
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("first receipt");
    write_receipt(&receipts, &first, /*replace*/ false).expect("write first receipt");
    assert!(compare_current(&receipts, "team/build", &ExpectedCurrent::Absent).is_err());
    let expected = ExpectedCurrent::Receipt(
        ReceiptIdentity::from_receipt(&first).expect("first receipt identity"),
    );
    assert_eq!(
        compare_current(&receipts, "team/build", &expected).expect("unchanged receipt"),
        Some(first.clone())
    );

    let mut second = first.clone();
    second.policy = WorkflowUpdatePolicy::Manual;
    write_receipt(&receipts, &second, /*replace*/ true).expect("replace receipt");
    assert!(compare_current(&receipts, "team/build", &expected).is_err());
    assert_eq!(
        compare_current(
            &receipts,
            "team/build",
            &ExpectedCurrent::Receipt(
                ReceiptIdentity::from_receipt(&second).expect("second receipt identity"),
            ),
        )
        .expect("current receipt"),
        Some(second)
    );
}
