use std::fs;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn staging(root: &std::path::Path) -> SecureDirectory {
    let absolute = AbsolutePathBuf::from_absolute_path_checked(root).expect("absolute root");
    SecureDirectory::open_root(&absolute)
        .expect("open root")
        .child("staging")
        .expect("private staging")
}

#[test]
fn windows_staging_binds_record_and_cleans_on_guard_drop() {
    let root = tempfile::tempdir().expect("root");
    let parent = staging(root.path());
    let pending = TransactionStaging::create(&parent).expect("transaction staging");
    let name = pending.name().to_owned();
    let (device, inode) = pending.directory().identity().expect("identity");
    assert_eq!(
        fs::read(
            parent
                .path()
                .join(cleanup::ownership_record_name(&name))
                .as_path()
        )
        .expect("ownership record"),
        cleanup::bound_record(&name, device, inode)
    );
    assert_eq!(
        fs::read(
            pending
                .directory()
                .path()
                .join(".codex-managed-operation")
                .as_path()
        )
        .expect("operation marker"),
        name.as_bytes()
    );
    pending
        .directory()
        .child("payload")
        .expect("payload")
        .write_file("file", b"contents", /*replace*/ false)
        .expect("payload file");
    drop(pending);
    assert!(!parent.path().join(&name).exists());
    assert!(
        !parent
            .path()
            .join(cleanup::ownership_record_name(&name))
            .exists()
    );
}

#[test]
fn retained_windows_staging_can_be_recovered_after_guard_drop() {
    let root = tempfile::tempdir().expect("root");
    let parent = staging(root.path());
    let mut pending = TransactionStaging::create(&parent).expect("transaction staging");
    let name = pending.name().to_owned();
    let (device, inode) = pending.directory().identity().expect("identity");
    pending.retain_for_recovery();
    drop(pending);
    assert!(parent.path().join(&name).is_dir());
    cleanup::remove_tree(
        &parent,
        &name,
        device,
        inode,
        cleanup::OwnershipMarker::Required,
        cleanup::CleanupEntryLimit::STANDARD,
    )
    .expect("recover marked staging");
    assert!(!parent.path().join(&name).exists());
}
