use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

#[cfg(any(unix, windows))]
use super::cleanup;
#[cfg(any(unix, windows))]
use super::fs::SecureDirectory;
#[cfg(any(unix, windows))]
use super::lock::LockMode;
#[cfg(any(unix, windows))]
use super::lock::ManagedFileLock;

/// Retains a marked Bun operation directory until every prepared command exits.
pub(in crate::managed) struct ManagedBunOperationDirectory {
    path: AbsolutePathBuf,
    #[cfg(any(unix, windows))]
    parent: SecureDirectory,
    #[cfg(any(unix, windows))]
    name: String,
    #[cfg(any(unix, windows))]
    device: u64,
    #[cfg(any(unix, windows))]
    inode: u64,
    #[cfg(any(unix, windows))]
    _global: ManagedFileLock,
    #[cfg(not(any(unix, windows)))]
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
    #[cfg(windows)]
    pub(in crate::managed) fn create_with_layout(
        management_root: &AbsolutePathBuf,
        bunfig: &[u8],
    ) -> anyhow::Result<Self> {
        let management = SecureDirectory::open_root(management_root)?;
        let global = management.open_lock_file("managed.lock")?;
        let _layout_lock =
            ManagedFileLock::acquire(global, LockMode::Shared, /*cancelled*/ None)?;
        let bun = management.child("bun")?;
        bun.child("cache")?;
        let operations = bun.child("operations")?;
        let operation = Self::create(operations.path())?;
        let root = operations.existing_child(&operation.name)?;
        root.child("scratch")?;
        root.child("temp")?;
        let home = root.child("home")?;
        for name in [
            "xdg-config",
            "xdg-cache",
            "xdg-data",
            "xdg-state",
            "app-data",
            "local-app-data",
        ] {
            home.child(name)?;
        }
        root.write_file("bunfig.toml", bunfig, /*replace*/ false)?;
        root.write_file("npmrc", b"", /*replace*/ false)?;
        Ok(operation)
    }

    pub(in crate::managed) fn create(operations: &AbsolutePathBuf) -> anyhow::Result<Self> {
        #[cfg(any(unix, windows))]
        let management = operations
            .as_path()
            .parent()
            .and_then(Path::parent)
            .context("managed Bun operation root has no management parent")?;
        #[cfg(any(unix, windows))]
        let management = AbsolutePathBuf::from_absolute_path_checked(management)?;
        #[cfg(any(unix, windows))]
        let management = SecureDirectory::open_root(&management)?;
        #[cfg(any(unix, windows))]
        let global = management.open_lock_file("managed.lock")?;
        #[cfg(any(unix, windows))]
        let global = ManagedFileLock::acquire(global, LockMode::Shared, /*cancelled*/ None)?;

        #[cfg(unix)]
        {
            let mut builder = tempfile::Builder::new();
            builder.prefix("operation-");
            use std::os::unix::fs::PermissionsExt;

            let private = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(operations.as_path(), private.clone())
                .context("failed to secure managed Bun operation root")?;
            builder.permissions(private);
            let temporary = builder
                .tempdir_in(operations.as_path())
                .context("failed to create private managed Bun operation directory")?;
            let path = AbsolutePathBuf::from_absolute_path_checked(temporary.path())
                .context("managed Bun operation directory was not absolute")?;
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
                &cleanup::bound_record(&name, metadata.st_dev, metadata.st_ino),
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
                device: metadata.st_dev,
                inode: metadata.st_ino,
                _global: global,
            })
        }
        #[cfg(windows)]
        {
            use std::sync::atomic::AtomicU64;
            use std::time::SystemTime;
            use std::time::UNIX_EPOCH;

            static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(0);
            let bun = management.existing_child("bun")?;
            let parent = bun.existing_child("operations")?;
            if parent.path() != operations {
                bail!("managed Bun operation path does not match private root");
            }
            let clock = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock predates Unix epoch")?
                .as_nanos();
            for _ in 0..128 {
                let sequence = NEXT_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
                let name = format!("operation-{}-{clock}-{sequence}", std::process::id());
                if parent.optional_existing_child(&name)?.is_some() {
                    continue;
                }
                let record = cleanup::ownership_record_name(&name);
                parent.write_file(
                    &record,
                    &cleanup::reservation_record(&name),
                    /*replace*/ false,
                )?;
                let directory = parent
                    .create_new_child(&name)
                    .context("new Bun operation remains reserved for recovery")?;
                let (device, inode) = directory.identity()?;
                let result = (|| {
                    parent.write_file(
                        &record,
                        &cleanup::bound_record(&name, device, inode),
                        /*replace*/ true,
                    )?;
                    directory.write_file(
                        ".codex-managed-operation",
                        name.as_bytes(),
                        /*replace*/ false,
                    )
                })();
                drop(directory);
                if let Err(error) = result {
                    let _ = cleanup::remove_tree(
                        &parent,
                        &name,
                        device,
                        inode,
                        cleanup::OwnershipMarker::CreationIncomplete {
                            original_device: device,
                            original_inode: inode,
                        },
                        cleanup::CleanupEntryLimit::STANDARD,
                    );
                    return Err(error).context("failed to mark private Bun operation");
                }
                return Ok(Self {
                    path: operations.join(&name),
                    parent,
                    name,
                    device,
                    inode,
                    _global: global,
                });
            }
            bail!("failed to allocate private Bun operation directory");
        }
        #[cfg(not(any(unix, windows)))]
        {
            let temporary = tempfile::Builder::new()
                .prefix("operation-")
                .tempdir_in(operations.as_path())?;
            let path = AbsolutePathBuf::from_absolute_path_checked(temporary.path())?;
            Ok(Self { path, temporary })
        }
    }

    pub(in crate::managed) fn path(&self) -> &Path {
        self.path.as_path()
    }
}

#[cfg(any(unix, windows))]
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

#[cfg(any(unix, windows))]
pub(super) fn recover_marked_bun_operations(
    management: &SecureDirectory,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
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
        let (device, inode) = match operations.optional_existing_child(&name)? {
            Some(root) => root.identity()?,
            None => (0, 0),
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

#[cfg(all(test, windows))]
#[path = "operation_windows_tests.rs"]
mod windows_tests;
