use std::fs;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::ManagedWorkflowStore;
use super::*;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

fn store(root: &Path) -> ManagedWorkflowStore {
    fs::create_dir(root.join("workflows")).expect("workflow root");
    ManagedWorkflowStore::create(&absolute(root), &absolute(&root.join("workflows")))
        .expect("managed store")
}

#[test]
fn bun_operation_has_bound_record_and_cleans_on_last_drop() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let operations = store
        .management
        .child("bun")
        .expect("bun root")
        .child("operations")
        .expect("operation root");
    let operation =
        ManagedBunOperationDirectory::create(operations.path()).expect("marked Bun operation");
    let name = operation
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("operation name")
        .to_owned();
    assert_eq!(
        fs::read(operation.path().join(".codex-managed-operation")).expect("operation marker"),
        name.as_bytes()
    );
    assert!(
        operations
            .path()
            .join(cleanup::ownership_record_name(&name))
            .is_file()
    );
    drop(operation);
    assert!(!operations.path().join(&name).exists());
    assert!(
        !operations
            .path()
            .join(cleanup::ownership_record_name(&name))
            .exists()
    );
}

#[test]
fn startup_removes_only_marked_orphans_and_keeps_cache() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let bun = store.management.child("bun").expect("bun root");
    let operations = bun.child("operations").expect("operation root");
    let cache = bun.child("cache").expect("persistent cache");
    cache
        .write_file("keep", b"cache", /*replace*/ false)
        .expect("cache file");
    let marked = operations.child("operation-marked").expect("marked orphan");
    let metadata = rustix::fs::fstat(marked.handle()).expect("orphan identity");
    operations
        .write_file(
            &cleanup::ownership_record_name("operation-marked"),
            &cleanup::bound_record("operation-marked", metadata.st_dev, metadata.st_ino),
            /*replace*/ false,
        )
        .expect("ownership record");
    marked
        .write_file(
            ".codex-managed-operation",
            b"operation-marked",
            /*replace*/ false,
        )
        .expect("inner marker");
    marked
        .child("nested")
        .expect("nested")
        .write_file("file", b"payload", /*replace*/ false)
        .expect("orphan payload");
    operations
        .child("operation-unknown")
        .expect("unmarked directory");
    operations
        .write_file("unknown-file", b"keep", /*replace*/ false)
        .expect("unknown file");
    drop(store);
    let _restarted = ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("startup recovery");
    assert!(!operations.path().join("operation-marked").exists());
    assert!(operations.path().join("operation-unknown").is_dir());
    assert!(operations.path().join("unknown-file").is_file());
    assert_eq!(
        fs::read(cache.path().join("keep")).expect("cache preserved"),
        b"cache"
    );
}

#[test]
fn startup_waits_for_a_live_bun_operation() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let operations = store
        .management
        .child("bun")
        .expect("bun root")
        .child("operations")
        .expect("operation root");
    let operation =
        ManagedBunOperationDirectory::create(operations.path()).expect("live operation");
    let exclusive = store
        .management
        .open_lock_file("managed.lock")
        .expect("probe global lock");
    assert_eq!(
        rustix::fs::flock(
            &exclusive,
            rustix::fs::FlockOperation::NonBlockingLockExclusive
        ),
        Err(rustix::io::Errno::WOULDBLOCK)
    );
    drop(operation);
    rustix::fs::flock(
        &exclusive,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("exclusive lock available after operation");
    drop(exclusive);
    ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("startup after operation");
}
