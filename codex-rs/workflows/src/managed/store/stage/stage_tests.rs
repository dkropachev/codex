#![allow(clippy::expect_used)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

fn staging_root(root: &Path) -> SecureDirectory {
    let root = AbsolutePathBuf::from_absolute_path_checked(root).expect("absolute root");
    SecureDirectory::open_root(&root)
        .expect("open root")
        .child("staging")
        .expect("private staging root")
}

#[test]
fn drops_private_tree_without_following_dependency_links() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let external = tempfile::tempdir().expect("external");
    fs::write(external.path().join("keep"), b"keep").expect("external file");
    let staging = staging_root(root.path());
    let pending = TransactionStaging::create(&staging).expect("transaction staging");
    let name = pending.name().to_owned();
    let metadata = fs::metadata(pending.directory().path().as_path()).expect("staging identity");
    assert_eq!(
        fs::read(staging.path().join(cleanup::ownership_record_name(&name)))
            .expect("bound ownership record"),
        cleanup::bound_record(&name, metadata.dev(), metadata.ino()),
    );
    assert_eq!(
        fs::read(pending.directory().path().join(".codex-managed-operation"))
            .expect("ownership marker"),
        name.as_bytes(),
    );
    let nested = pending
        .directory()
        .child("nested")
        .expect("nested directory");
    nested
        .write_file("file", b"contents", /*replace*/ false)
        .expect("staged file");
    symlink(external.path(), nested.path().join("alias").as_path()).expect("external alias");
    drop(pending);
    assert!(!staging.path().join(&name).exists());
    assert!(
        !staging
            .path()
            .join(cleanup::ownership_record_name(&name))
            .exists()
    );
    assert_eq!(
        fs::read(external.path().join("keep")).expect("external unchanged"),
        b"keep"
    );
}

#[test]
fn retained_staging_survives_guard_drop_for_recovery() {
    let root = tempfile::tempdir().expect("root");
    let staging = staging_root(root.path());
    let mut pending = TransactionStaging::create(&staging).expect("transaction staging");
    let name = pending.name().to_owned();
    pending.retain_for_recovery();
    drop(pending);
    assert!(staging.path().join(&name).is_dir());
    assert!(
        staging
            .path()
            .join(cleanup::ownership_record_name(&name))
            .is_file()
    );
}

#[test]
fn cleanup_refuses_replaced_staging_identity() {
    let root = tempfile::tempdir().expect("root");
    let staging = staging_root(root.path());
    let mut pending = TransactionStaging::create(&staging).expect("transaction staging");
    let original = staging.path().join(pending.name());
    let moved = root.path().join("moved");
    fs::rename(original.as_path(), &moved).expect("move staging root");
    fs::create_dir(original.as_path()).expect("replace staging name");
    let error = pending.cleanup().expect_err("refuse replaced directory");
    assert!(error.to_string().contains("changed identity"), "{error:#}");
    drop(pending);
    assert!(original.is_dir());
    assert!(moved.is_dir());
}
