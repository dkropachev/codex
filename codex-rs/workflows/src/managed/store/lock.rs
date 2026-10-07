use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;

#[derive(Clone, Copy)]
pub(super) enum LockMode {
    Shared,
    Exclusive,
}

/// A permanent lock file remains on disk after this held lock is dropped.
pub(super) struct ManagedFileLock {
    _file: fs::File,
}

impl ManagedFileLock {
    pub(super) fn acquire(
        file: fs::File,
        mode: LockMode,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::FlockOperation;
            use rustix::fs::flock;
            use rustix::io::Errno;

            let operation = match mode {
                LockMode::Shared => FlockOperation::NonBlockingLockShared,
                LockMode::Exclusive => FlockOperation::NonBlockingLockExclusive,
            };
            loop {
                if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
                    bail!("managed workflow lock acquisition was cancelled");
                }
                match flock(&file, operation) {
                    Ok(()) => return Ok(Self { _file: file }),
                    Err(Errno::WOULDBLOCK) => {
                        std::thread::sleep(Duration::from_millis(/*millis*/ 10))
                    }
                    Err(error) => {
                        return Err(error).context("failed to acquire managed workflow lock");
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (file, mode, cancelled);
            bail!("managed workflow locking is not yet available on this platform");
        }
    }
}
