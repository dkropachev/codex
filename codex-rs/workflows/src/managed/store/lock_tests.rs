use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

#[test]
fn windows_global_lock_supports_shared_exclusive_and_cancellation() {
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("managed.lock");
    let open = || {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .expect("lock file")
    };
    let first = ManagedFileLock::acquire(open(), LockMode::Shared, /*cancelled*/ None)
        .expect("first shared lock");
    let second = ManagedFileLock::acquire(open(), LockMode::Shared, /*cancelled*/ None)
        .expect("second shared lock");
    assert!(matches!(
        open().try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    let cancelled = Arc::new(AtomicBool::new(false));
    let waiter_signal = Arc::clone(&cancelled);
    let waiter_path = path.clone();
    let (sender, receiver) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(waiter_path)
            .expect("waiter lock file");
        sender.send("started").expect("waiter start");
        ManagedFileLock::acquire(file, LockMode::Exclusive, Some(&waiter_signal))
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(/*secs*/ 1))
            .expect("waiter start"),
        "started"
    );
    thread::sleep(Duration::from_millis(/*millis*/ 30));
    cancelled.store(true, Ordering::Relaxed);
    assert!(waiter.join().expect("waiter thread").is_err());
    drop(first);
    drop(second);
    let exclusive = ManagedFileLock::acquire(open(), LockMode::Exclusive, /*cancelled*/ None)
        .expect("exclusive lock");
    assert!(matches!(
        open().try_lock_shared(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(exclusive);
    ManagedFileLock::acquire(open(), LockMode::Shared, /*cancelled*/ None)
        .expect("shared lock after exclusive release");
}
