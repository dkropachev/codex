use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn windows_private_store_creates_reads_replaces_and_renames() {
    let root = tempfile::tempdir().expect("root");
    let root = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("absolute root");
    let parent = SecureDirectory::open_root(&root).expect("open root");
    let source = parent.child("source").expect("private source directory");
    assert_eq!(
        parent.device_id().expect("root volume"),
        source.device_id().expect("child volume")
    );
    source
        .write_file("receipt.json", b"first", /*replace*/ false)
        .expect("write private file");
    assert_eq!(
        source
            .read_file("receipt.json", /*maximum_bytes*/ 16)
            .expect("read private file"),
        b"first"
    );
    source
        .write_file("receipt.json", b"second", /*replace*/ true)
        .expect("replace private file");
    assert_eq!(
        source
            .read_file("receipt.json", /*maximum_bytes*/ 16)
            .expect("read replacement"),
        b"second"
    );
    assert!(
        source
            .write_file("receipt.json", b"third", /*replace*/ false)
            .is_err()
    );
    let held_lock = source
        .open_lock_file("managed.lock")
        .expect("private lock file");
    super::super::windows_security::open_private_file(source.path().join("receipt.json").as_path())
        .expect("protected receipt ACL");
    super::super::windows_security::open_private_file(source.path().join("managed.lock").as_path())
        .expect("protected lock ACL");
    source
        .open_lock_file("managed.lock")
        .expect("reopen retained lock");
    drop(held_lock);
    std::fs::write(source.path().join("permissive.lock").as_path(), b"unsafe")
        .expect("create inherited lock fixture");
    assert!(source.open_lock_file("permissive.lock").is_err());
    assert!(
        parent
            .optional_existing_child("missing")
            .expect("optional read")
            .is_none()
    );
    assert!(
        parent
            .optional_existing_child("source")
            .expect("existing private child")
            .is_some()
    );
    let destination = parent.child("destination").expect("destination directory");
    drop(source);
    parent
        .rename_child_noreplace("source", &destination, "moved")
        .expect("atomic directory rename");
    assert_eq!(
        destination
            .existing_child("moved")
            .expect("renamed directory")
            .read_file("receipt.json", /*maximum_bytes*/ 16)
            .expect("renamed file"),
        b"second"
    );
}
