use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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
fn windows_bun_operation_holds_global_lock_and_cleans_on_drop() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let operation =
        ManagedBunOperationDirectory::create_with_layout(store.management.path(), b"trusted")
            .expect("marked operation");
    let name = operation
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .expect("operation name")
        .to_owned();
    let operations = store
        .management
        .existing_child("bun")
        .expect("bun root")
        .existing_child("operations")
        .expect("operation root");
    assert_eq!(
        fs::read(operation.path().join(".codex-managed-operation")).expect("marker"),
        name.as_bytes()
    );
    assert!(operation.path().join("home/xdg-cache").is_dir());
    assert!(
        operations
            .path()
            .join(cleanup::ownership_record_name(&name))
            .is_file()
    );
    let probe = store
        .management
        .open_lock_file("managed.lock")
        .expect("lock probe");
    assert!(matches!(
        probe.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(operation);
    probe.try_lock().expect("exclusive lock after operation");
    drop(probe);
    assert!(!operations.path().join(&name).exists());
    assert!(
        !operations
            .path()
            .join(cleanup::ownership_record_name(&name))
            .exists()
    );
}

#[test]
fn windows_startup_removes_only_bound_bun_orphans() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let operation =
        ManagedBunOperationDirectory::create_with_layout(store.management.path(), b"trusted")
            .expect("initialize Bun roots");
    drop(operation);
    let bun = store.management.existing_child("bun").expect("bun root");
    let operations = bun.existing_child("operations").expect("operation root");
    let cache = bun.existing_child("cache").expect("persistent cache");
    cache
        .write_file("keep", b"cache", /*replace*/ false)
        .expect("cache file");
    let orphan = operations
        .create_new_child("operation-orphan")
        .expect("owned orphan");
    let (device, inode) = orphan.identity().expect("identity");
    operations
        .write_file(
            &cleanup::ownership_record_name("operation-orphan"),
            &cleanup::bound_record("operation-orphan", device, inode),
            /*replace*/ false,
        )
        .expect("record");
    orphan
        .write_file(
            ".codex-managed-operation",
            b"operation-orphan",
            /*replace*/ false,
        )
        .expect("inner marker");
    orphan
        .child("nested")
        .expect("nested directory")
        .write_file("file", b"payload", /*replace*/ false)
        .expect("payload");
    operations
        .create_new_child("operation-unknown")
        .expect("unmarked directory");
    drop((orphan, operations, cache, bun, store));

    let recovered = ManagedWorkflowStore::create(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("startup orphan cleanup");
    let operations = recovered
        .management
        .existing_child("bun")
        .expect("bun root")
        .existing_child("operations")
        .expect("operation root");
    assert!(!operations.path().join("operation-orphan").exists());
    assert!(operations.path().join("operation-unknown").is_dir());
    assert_eq!(
        fs::read(recovered.management.path().join("bun/cache/keep")).expect("cache"),
        b"cache"
    );
}

#[test]
fn windows_bun_layout_waits_for_global_recovery_lock() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let recovery = store
        .lock_recovery(/*cancelled*/ None)
        .expect("exclusive recovery lock");
    let management = store.management.path().clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        sender.send("started").expect("start signal");
        ManagedBunOperationDirectory::create_with_layout(&management, b"trusted")
            .expect("Bun layout after lock release")
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(/*secs*/ 1))
            .expect("worker start"),
        "started"
    );
    thread::sleep(Duration::from_millis(/*millis*/ 50));
    assert!(!store.management.path().join("bun").exists());
    drop(recovery);
    drop(worker.join().expect("Bun layout worker"));
    assert!(store.management.path().join("bun/cache").is_dir());
}

#[test]
fn windows_bun_layout_creates_an_absent_private_management_root() {
    let root = tempfile::tempdir().expect("root");
    let management = absolute(&root.path().join(".workflow-management"));
    assert!(!management.as_path().exists());
    let operation = ManagedBunOperationDirectory::create_with_layout(&management, b"trusted")
        .expect("create missing management root and Bun layout");
    assert!(management.join("bun/cache").is_dir());
    assert!(operation.path().join("bunfig.toml").is_file());
    drop(operation);
    assert!(management.join("managed.lock").is_file());
}
