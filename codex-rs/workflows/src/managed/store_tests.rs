use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::ManagedWorkflowStore;
use super::fs::SecureDirectory;

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

#[test]
fn private_directories_and_atomic_files_are_reusable() {
    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let metadata = root
        .child(".workflow-management")
        .expect("management directory");
    let receipts = metadata.child("receipts").expect("receipts directory");
    receipts
        .write_file("receipt.json", b"first", /*replace*/ false)
        .expect("create receipt");
    assert_eq!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 5)
            .expect("read receipt"),
        b"first"
    );
    assert!(
        receipts
            .write_file("receipt.json", b"collision", /*replace*/ false)
            .is_err()
    );
    receipts
        .write_file("receipt.json", b"next", /*replace*/ true)
        .expect("replace receipt");
    assert_eq!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 4)
            .expect("read replaced receipt"),
        b"next"
    );
    assert!(
        receipts
            .read_file("receipt.json", /*maximum_bytes*/ 3)
            .is_err()
    );
    assert!(metadata.child("receipts").is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(metadata.path().as_path())
                .expect("directory mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(receipts.path().join("receipt.json").as_path())
                .expect("file mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn no_replace_directory_rename_preserves_existing_target() {
    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let staging = root.child("staging").expect("staging");
    let active = root.child("active").expect("active");
    staging.child("candidate").expect("candidate");
    staging
        .rename_child_noreplace("candidate", &active, "workflow")
        .expect("publish candidate");
    assert!(active.path().join("workflow").is_dir());
    staging.child("another").expect("another candidate");
    assert!(
        staging
            .rename_child_noreplace("another", &active, "workflow")
            .is_err()
    );
    assert!(staging.path().join("another").is_dir());
}

#[cfg(unix)]
#[test]
fn rejects_aliased_metadata_and_world_accessible_directory() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let root = SecureDirectory::open_root(&absolute(root.path())).expect("open root");
    let private = root.child("private").expect("private");
    private
        .write_file("real", b"contents", /*replace*/ false)
        .expect("real file");
    symlink("real", private.path().join("alias").as_path()).expect("alias");
    assert!(private.read_file("alias", /*maximum_bytes*/ 8).is_err());
    let status = std::process::Command::new("mkfifo")
        .arg(private.path().join("fifo").as_path())
        .status()
        .expect("create FIFO");
    assert!(status.success(), "mkfifo failed");
    assert!(private.read_file("fifo", /*maximum_bytes*/ 8).is_err());
    fs::set_permissions(private.path().as_path(), fs::Permissions::from_mode(0o755))
        .expect("relax directory mode");
    assert!(root.child("private").is_err());
}

fn store(root: &Path) -> ManagedWorkflowStore {
    fs::create_dir(root.join("workflows")).expect("workflow root");
    ManagedWorkflowStore::create(&absolute(root), &absolute(&root.join("workflows")))
        .expect("managed store")
}

#[test]
fn store_creates_private_layout_and_permanent_locks() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let locked = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("install lock");
    assert_eq!(locked.id, "team/build");
    drop(locked);
    let management = root.path().join(".workflow-management");
    for name in ["locks", "receipts", "journals", "staging", "backups"] {
        assert!(management.join(name).is_dir(), "{name}");
    }
    assert!(management.join("managed.lock").is_file());
    assert!(management.join("locks/team/build.lock").is_file());
}

#[test]
fn same_id_install_waits_while_other_ids_and_shared_runs_proceed() {
    let root = tempfile::tempdir().expect("root");
    let store = Arc::new(store(root.path()));
    let held = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("hold install lock");
    store
        .lock_install("team/other", /*cancelled*/ None)
        .expect("different ID proceeds");
    let (started_tx, started_rx) = mpsc::channel();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let other = Arc::clone(&store);
    let thread = std::thread::spawn(move || {
        started_tx.send(()).expect("signal start");
        let lock = other
            .lock_install("team/build", /*cancelled*/ None)
            .expect("wait for install lock");
        acquired_tx.send(()).expect("signal acquisition");
        drop(lock);
    });
    started_rx
        .recv_timeout(Duration::from_secs(/*secs*/ 2))
        .expect("thread started");
    assert!(
        acquired_rx
            .recv_timeout(Duration::from_millis(/*millis*/ 30))
            .is_err()
    );
    drop(held);
    acquired_rx
        .recv_timeout(Duration::from_secs(/*secs*/ 2))
        .expect("same ID acquired after release");
    thread.join().expect("join lock thread");
    let run = store
        .lock_run("team/build", /*cancelled*/ None)
        .expect("first shared run");
    let other_run = store
        .lock_run("team/build", /*cancelled*/ None)
        .expect("second shared run");
    let (install_tx, install_rx) = mpsc::channel();
    let other = Arc::clone(&store);
    let waiting_install = std::thread::spawn(move || {
        let lock = other
            .lock_install("team/build", /*cancelled*/ None)
            .expect("wait for runs");
        install_tx.send(()).expect("signal install acquisition");
        drop(lock);
    });
    assert!(
        install_rx
            .recv_timeout(Duration::from_millis(/*millis*/ 30))
            .is_err()
    );
    store
        .lock_install("team/other", /*cancelled*/ None)
        .expect("different ID still proceeds");
    drop((run, other_run));
    install_rx
        .recv_timeout(Duration::from_secs(/*secs*/ 2))
        .expect("install after runs");
    waiting_install.join().expect("join install waiter");
}

#[test]
fn cancellation_stops_waiting_for_global_recovery_lock() {
    let root = tempfile::tempdir().expect("root");
    let store = Arc::new(store(root.path()));
    let held = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("hold install lock");
    let cancelled = Arc::new(AtomicBool::new(false));
    let other = Arc::clone(&store);
    let signal = Arc::clone(&cancelled);
    let thread = std::thread::spawn(move || other.lock_recovery(Some(&signal)).map(drop));
    std::thread::sleep(Duration::from_millis(/*millis*/ 30));
    cancelled.store(true, Ordering::Relaxed);
    let error = thread
        .join()
        .expect("join recovery waiter")
        .expect_err("recovery cancelled");
    assert!(error.to_string().contains("cancelled"));
    drop(held);
    store
        .lock_recovery(/*cancelled*/ None)
        .expect("recovery lock after release");
}

#[test]
fn spawned_process_does_not_retain_a_released_lock() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let held = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("hold lock");
    let mut child = std::process::Command::new("sh")
        .args(["-c", "sleep 1"])
        .spawn()
        .expect("spawn child while lock held");
    drop(held);
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = Arc::clone(&cancelled);
    let watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(/*millis*/ 100));
        signal.store(true, Ordering::Relaxed);
    });
    let lock = store
        .lock_install("team/build", Some(&cancelled))
        .expect("child must not inherit lock");
    drop(lock);
    watchdog.join().expect("join watchdog");
    child.wait().expect("wait child");
}

#[cfg(target_os = "linux")]
#[test]
fn rejects_cross_device_roots_before_creating_metadata() {
    use std::os::unix::fs::MetadataExt;

    let home = tempfile::tempdir().expect("home");
    let Ok(workflows) = tempfile::tempdir_in("/dev/shm") else {
        return;
    };
    if fs::metadata(home.path()).expect("home device").dev()
        == fs::metadata(workflows.path())
            .expect("workflow device")
            .dev()
    {
        return;
    }
    assert!(
        ManagedWorkflowStore::create(&absolute(home.path()), &absolute(workflows.path())).is_err()
    );
    assert!(!home.path().join(".workflow-management").exists());
}
