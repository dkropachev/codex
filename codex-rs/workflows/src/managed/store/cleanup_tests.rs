#![allow(clippy::expect_used)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;

use super::*;

fn private_root(path: &Path) -> SecureDirectory {
    let path = AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute root");
    SecureDirectory::open_root(&path)
        .expect("open root")
        .child("staging")
        .expect("private staging")
}

fn marked(parent: &SecureDirectory, name: &str) -> SecureDirectory {
    parent
        .write_file(
            &ownership_record_name(name),
            &reservation_record(name),
            /*replace*/ false,
        )
        .expect("sibling ownership record");
    let owned = parent.child(name).expect("owned directory");
    let (device, inode) = identity(&owned);
    parent
        .write_file(
            &ownership_record_name(name),
            &bound_record(name, device, inode),
            /*replace*/ true,
        )
        .expect("bind ownership record to directory");
    owned
        .write_file(
            ".codex-managed-operation",
            name.as_bytes(),
            /*replace*/ false,
        )
        .expect("ownership marker");
    owned
}

fn identity(directory: &SecureDirectory) -> (u64, u64) {
    let metadata = fs::metadata(directory.path().as_path()).expect("directory metadata");
    (metadata.dev(), metadata.ino())
}

#[test]
fn counts_operation_overhead_and_keeps_marker_for_retry() {
    let root = tempfile::tempdir().expect("root");
    let parent = private_root(root.path());
    let owned = marked(&parent, "tx-boundary");
    owned.child("payload").expect("payload directory");
    let (device, inode) = identity(&owned);
    assert!(
        remove_tree(
            &parent,
            "tx-boundary",
            device,
            inode,
            OwnershipMarker::Required,
            CleanupEntryLimit(1),
        )
        .is_err()
    );
    assert!(owned.path().join(".codex-managed-operation").is_file());
    assert!(
        parent
            .path()
            .join(ownership_record_name("tx-boundary"))
            .is_file()
    );
    remove_tree(
        &parent,
        "tx-boundary",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit(2),
    )
    .expect("exact marker plus payload limit");

    let owned = marked(&parent, "tx-partial");
    let payload = owned.child("payload").expect("payload directory");
    payload
        .write_file("zfile", b"first", /*replace*/ false)
        .expect("first file");
    let nested = payload.child("+subdir").expect("nested directory");
    nested
        .write_file("inner", b"second", /*replace*/ false)
        .expect("inner file");
    let (device, inode) = identity(&owned);
    assert!(
        remove_tree(
            &parent,
            "tx-partial",
            device,
            inode,
            OwnershipMarker::Required,
            CleanupEntryLimit(4),
        )
        .is_err()
    );
    assert!(owned.path().join(".codex-managed-operation").is_file());
    assert!(
        parent
            .path()
            .join(ownership_record_name("tx-partial"))
            .is_file()
    );
    remove_tree(
        &parent,
        "tx-partial",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit::STANDARD,
    )
    .expect("retry partial cleanup");
    assert!(!parent.path().join("tx-partial").exists());
}

#[test]
fn sibling_record_allows_retry_after_inner_marker_was_removed() {
    let root = tempfile::tempdir().expect("root");
    let parent = private_root(root.path());
    let owned = marked(&parent, "tx-interrupted");
    let (device, inode) = identity(&owned);
    fs::remove_file(owned.path().join(".codex-managed-operation").as_path())
        .expect("simulate crash after inner marker removal");
    remove_tree(
        &parent,
        "tx-interrupted",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit::STANDARD,
    )
    .expect("resume cleanup through sibling record");
    assert!(!parent.path().join("tx-interrupted").exists());
    assert!(
        !parent
            .path()
            .join(ownership_record_name("tx-interrupted"))
            .exists()
    );
}

#[test]
fn stale_record_cannot_authorize_replacement_directory() {
    let root = tempfile::tempdir().expect("root");
    let parent = private_root(root.path());
    let owned = marked(&parent, "tx-original");
    let moved = root.path().join("moved");
    fs::rename(owned.path().as_path(), &moved).expect("move owned directory");
    let replacement = parent.child("tx-original").expect("replacement directory");
    let (device, inode) = identity(&replacement);
    let error = remove_tree(
        &parent,
        "tx-original",
        device,
        inode,
        OwnershipMarker::Required,
        CleanupEntryLimit::STANDARD,
    )
    .expect_err("reject stale ownership record");
    assert!(
        error.to_string().contains("directory identity"),
        "{error:#}"
    );
    assert!(replacement.path().is_dir() && moved.is_dir());
}

#[test]
fn reservation_requires_the_original_directory_handle() {
    let root = tempfile::tempdir().expect("root");
    let parent = private_root(root.path());
    parent
        .write_file(
            &ownership_record_name("tx-reserved"),
            &reservation_record("tx-reserved"),
            /*replace*/ false,
        )
        .expect("reservation");
    let original = parent.child("tx-reserved").expect("original directory");
    let moved = root.path().join("moved");
    fs::rename(original.path().as_path(), &moved).expect("move original directory");
    let replacement = parent.child("tx-reserved").expect("replacement directory");
    let (device, inode) = identity(&replacement);
    let error = remove_tree(
        &parent,
        "tx-reserved",
        device,
        inode,
        OwnershipMarker::CreationIncomplete(original.handle()),
        CleanupEntryLimit::STANDARD,
    )
    .expect_err("reject replacement despite reservation");
    assert!(error.to_string().contains("creation proof"), "{error:#}");
    assert!(replacement.path().is_dir() && moved.is_dir());
}
