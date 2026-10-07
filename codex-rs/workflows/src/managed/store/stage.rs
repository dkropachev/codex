use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::bail;

#[cfg(unix)]
use super::cleanup;
use super::fs::SecureDirectory;

static NEXT_TRANSACTION_ID: AtomicU64 = AtomicU64::new(0);

/// Owns one private staging directory until a journal takes responsibility for it.
pub(super) struct TransactionStaging<'a> {
    pub(super) parent: &'a SecureDirectory,
    directory: SecureDirectory,
    name: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    armed: bool,
}

#[cfg(unix)]
struct CreationCleanup<'a> {
    parent: &'a SecureDirectory,
    name: String,
    device: u64,
    inode: u64,
    directory: &'a SecureDirectory,
    armed: bool,
}

#[cfg(unix)]
impl Drop for CreationCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = cleanup::remove_tree(
                self.parent,
                &self.name,
                self.device,
                self.inode,
                cleanup::OwnershipMarker::CreationIncomplete(self.directory.handle()),
                cleanup::CleanupEntryLimit::STANDARD,
            );
        }
    }
}

impl<'a> TransactionStaging<'a> {
    pub(super) fn create(parent: &'a SecureDirectory) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::AtFlags;
            use rustix::fs::Mode;
            use rustix::fs::mkdirat;
            use rustix::fs::unlinkat;
            use rustix::io::Errno;

            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock predates Unix epoch")?
                .as_nanos();
            for _ in 0..128 {
                let sequence = NEXT_TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
                let name = format!("tx-{}-{clock}-{sequence}", std::process::id());
                let record_name = cleanup::ownership_record_name(&name);
                parent.write_file(
                    &record_name,
                    &cleanup::reservation_record(&name),
                    /*replace*/ false,
                )?;
                match mkdirat(parent.handle(), name.as_str(), Mode::RWXU) {
                    Ok(()) => {}
                    Err(Errno::EXIST) => {
                        unlinkat(parent.handle(), record_name.as_str(), AtFlags::empty())?;
                        parent.sync()?;
                        continue;
                    }
                    Err(error) => {
                        unlinkat(parent.handle(), record_name.as_str(), AtFlags::empty())?;
                        parent.sync()?;
                        return Err(error).context("failed to create workflow transaction staging");
                    }
                }
                // If the new directory cannot be opened, its identity cannot be
                // proven. Keep the reservation and fail closed for recovery.
                let directory = parent
                    .existing_child(&name)
                    .context("new transaction staging remains reserved for recovery")?;
                let metadata = rustix::fs::fstat(directory.handle())
                    .context("new transaction staging remains reserved for recovery")?;
                let mut creation = CreationCleanup {
                    parent,
                    name: name.clone(),
                    device: metadata.st_dev,
                    inode: metadata.st_ino,
                    directory: &directory,
                    armed: true,
                };
                parent.sync()?;
                parent.write_file(
                    &record_name,
                    &cleanup::bound_record(&name, metadata.st_dev, metadata.st_ino),
                    /*replace*/ true,
                )?;
                directory.write_file(
                    ".codex-managed-operation",
                    name.as_bytes(),
                    /*replace*/ false,
                )?;
                creation.armed = false;
                drop(creation);
                return Ok(Self {
                    parent,
                    directory,
                    name,
                    device: metadata.st_dev,
                    inode: metadata.st_ino,
                    armed: true,
                });
            }
            bail!("failed to allocate workflow transaction staging directory");
        }
        #[cfg(not(unix))]
        {
            let _ = parent;
            bail!("secure workflow transaction staging is unavailable on this platform");
        }
    }

    pub(super) fn directory(&self) -> &SecureDirectory {
        &self.directory
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// Keeps staging for durable journal recovery after its journal is persisted.
    pub(super) fn retain_for_recovery(&mut self) {
        self.armed = false;
    }

    pub(super) fn cleanup(&mut self) -> anyhow::Result<()> {
        if self.armed {
            #[cfg(unix)]
            {
                cleanup::remove_tree(
                    self.parent,
                    &self.name,
                    self.device,
                    self.inode,
                    cleanup::OwnershipMarker::Required,
                    cleanup::CleanupEntryLimit::STANDARD,
                )?;
            }
            self.armed = false;
        }
        Ok(())
    }
}

impl Drop for TransactionStaging<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(all(test, unix))]
#[path = "stage/stage_tests.rs"]
mod tests;
