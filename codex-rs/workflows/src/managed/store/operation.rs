use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

#[cfg(unix)]
use super::cleanup;
#[cfg(unix)]
use super::fs::SecureDirectory;
#[cfg(unix)]
use super::fs::device_id_from_stat;
#[cfg(unix)]
use super::lock::LockMode;
#[cfg(unix)]
use super::lock::ManagedFileLock;

/// Retains a marked Bun operation directory until every prepared command exits.
pub(in crate::managed) struct ManagedBunOperationDirectory {
    path: AbsolutePathBuf,
    #[cfg(unix)]
    parent: SecureDirectory,
    #[cfg(unix)]
    name: String,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    _global: ManagedFileLock,
    #[cfg(not(unix))]
    temporary: tempfile::TempDir,
}

impl std::fmt::Debug for ManagedBunOperationDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedBunOperationDirectory")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ManagedBunOperationDirectory {
    pub(in crate::managed) fn create(operations: &AbsolutePathBuf) -> anyhow::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("operation-");
        #[cfg(unix)]
        let global = {
            let management = operations
                .as_path()
                .parent()
                .and_then(Path::parent)
                .context("managed Bun operation root has no management parent")?;
            let management = AbsolutePathBuf::from_absolute_path_checked(management)?;
            let management = SecureDirectory::open_root(&management)?;
            let global = management.open_lock_file("managed.lock")?;
            let global =
                ManagedFileLock::acquire(global, LockMode::Shared, /*cancelled*/ None)?;
            use std::os::unix::fs::PermissionsExt;

            let private = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(operations.as_path(), private.clone())
                .context("failed to secure managed Bun operation root")?;
            builder.permissions(private);
            global
        };
        let temporary = builder
            .tempdir_in(operations.as_path())
            .context("failed to create private managed Bun operation directory")?;
        let path = AbsolutePathBuf::from_absolute_path_checked(temporary.path())
            .context("managed Bun operation directory was not absolute")?;
        #[cfg(unix)]
        {
            let parent = SecureDirectory::open_root(operations)?;
            let name = temporary
                .path()
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .context("managed Bun operation name must be UTF-8")?
                .to_owned();
            let directory = parent.existing_child(&name)?;
            let metadata = rustix::fs::fstat(directory.handle())?;
            parent.write_file(
                &cleanup::ownership_record_name(&name),
                &cleanup::bound_record(
                    &name,
                    device_id_from_stat(metadata.st_dev),
                    metadata.st_ino,
                ),
                /*replace*/ false,
            )?;
            directory.write_file(
                ".codex-managed-operation",
                name.as_bytes(),
                /*replace*/ false,
            )?;
            let _ = temporary.keep();
            Ok(Self {
                path,
                parent,
                name,
                device: device_id_from_stat(metadata.st_dev),
                inode: metadata.st_ino,
                _global: global,
            })
        }
        #[cfg(not(unix))]
        Ok(Self { path, temporary })
    }

    pub(in crate::managed) fn path(&self) -> &Path {
        self.path.as_path()
    }
}

#[cfg(unix)]
impl Drop for ManagedBunOperationDirectory {
    fn drop(&mut self) {
        let _ = cleanup::remove_tree(
            &self.parent,
            &self.name,
            self.device,
            self.inode,
            cleanup::OwnershipMarker::Required,
            cleanup::CleanupEntryLimit::STANDARD,
        );
    }
}

#[cfg(unix)]
pub(super) fn recover_marked_bun_operations(
    management: &SecureDirectory,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    use rustix::fs::AtFlags;
    use rustix::fs::statat;
    use rustix::io::Errno;

    let Some(bun) = management.optional_existing_child("bun")? else {
        return Ok(());
    };
    let Some(operations) = bun.optional_existing_child("operations")? else {
        return Ok(());
    };
    for entry in operations.list_names(/*maximum_entries*/ 4_096, cancelled)? {
        if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
            bail!("managed Bun operation recovery was cancelled");
        }
        let Some(name) = entry
            .strip_prefix(".owner-operation-")
            .map(|suffix| format!("operation-{suffix}"))
        else {
            continue;
        };
        let root = statat(
            operations.handle(),
            name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        );
        let (device, inode) = match root {
            Ok(metadata) => (device_id_from_stat(metadata.st_dev), metadata.st_ino),
            Err(Errno::NOENT) => (0, 0),
            Err(error) => return Err(error).context("failed to inspect marked Bun operation"),
        };
        cleanup::remove_tree(
            &operations,
            &name,
            device,
            inode,
            cleanup::OwnershipMarker::Required,
            cleanup::CleanupEntryLimit::STANDARD,
        )?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "operation_tests.rs"]
mod tests;
