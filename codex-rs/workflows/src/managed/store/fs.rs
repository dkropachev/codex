use std::fs;
use std::io::Read;
use std::io::Write;
use std::sync::atomic::AtomicBool;
#[cfg(not(unix))]
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

pub(super) mod list;

#[cfg(windows)]
struct WindowsDirectoryGuard {
    handle: std::os::windows::io::OwnedHandle,
    _parent: Option<std::sync::Arc<WindowsDirectoryGuard>>,
}

/// Normalizes the platform-specific device ID for durable ownership records.
#[cfg(target_os = "macos")]
pub(super) fn device_id_from_stat(device: rustix::fs::Dev) -> u64 {
    device as u64
}

/// Normalizes the platform-specific device ID for durable ownership records.
#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn device_id_from_stat(device: rustix::fs::Dev) -> u64 {
    device
}

/// A retained, non-aliased directory used as the parent of managed metadata.
pub(super) struct SecureDirectory {
    path: AbsolutePathBuf,
    #[cfg(unix)]
    handle: std::os::fd::OwnedFd,
    #[cfg(windows)]
    guard: std::sync::Arc<WindowsDirectoryGuard>,
}

impl SecureDirectory {
    #[cfg(unix)]
    pub(super) fn handle(&self) -> &std::os::fd::OwnedFd {
        &self.handle
    }
    /// Opens an existing trusted root without following its final component.
    pub(super) fn open_root(path: &AbsolutePathBuf) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::open;

            let handle = open(
                path.as_path(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("failed to open managed workflow root")?;
            Ok(Self {
                path: path.clone(),
                handle,
            })
        }
        #[cfg(windows)]
        {
            use std::path::Component;
            use std::path::PathBuf;
            use std::sync::Arc;

            let mut current = PathBuf::new();
            let mut guard = None;
            for component in path.as_path().components() {
                current.push(component.as_os_str());
                if matches!(component, Component::Prefix(_) | Component::CurDir) {
                    continue;
                }
                if matches!(component, Component::ParentDir) {
                    bail!("managed workflow root must not contain parent components");
                }
                let handle = super::windows_security::open_directory(
                    &current, /*private*/ false, /*desired_access*/ 0,
                )?;
                guard = Some(Arc::new(WindowsDirectoryGuard {
                    handle,
                    _parent: guard,
                }));
            }
            Ok(Self {
                path: path.clone(),
                guard: guard.context("managed workflow root has no directory component")?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let metadata = fs::symlink_metadata(path.as_path())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("managed workflow root must be a regular directory");
            }
            Ok(Self { path: path.clone() })
        }
    }

    /// Creates or opens an owner-private child directory.
    pub(super) fn child(&self, name: &str) -> anyhow::Result<Self> {
        validate_component(name)?;
        let path = self.path.join(name);
        #[cfg(unix)]
        {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::mkdirat;
            use rustix::fs::openat;
            use rustix::io::Errno;

            let created = match mkdirat(&self.handle, name, Mode::RWXU) {
                Ok(()) => true,
                Err(Errno::EXIST) => false,
                Err(error) => {
                    return Err(error).context("failed to create private managed directory");
                }
            };
            let handle = openat(
                &self.handle,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("failed to open private managed directory")?;
            let metadata = rustix::fs::fstat(&handle)?;
            if metadata.st_mode & 0o077 != 0 {
                bail!("managed workflow directory is accessible to other users");
            }
            if created {
                self.sync()?;
            }
            Ok(Self { path, handle })
        }
        #[cfg(windows)]
        {
            let (handle, created) =
                super::windows_security::create_private_directory(path.as_path())?;
            if created {
                self.sync()?;
            }
            Ok(Self {
                path,
                guard: std::sync::Arc::new(WindowsDirectoryGuard {
                    handle,
                    _parent: Some(std::sync::Arc::clone(&self.guard)),
                }),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            fs::create_dir_all(path.as_path())?;
            let metadata = fs::symlink_metadata(path.as_path())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("managed workflow directory must not be aliased");
            }
            Ok(Self { path })
        }
    }

    pub(super) fn path(&self) -> &AbsolutePathBuf {
        &self.path
    }

    /// Lists a bounded set of child names; callers open each entry through this handle.
    pub(super) fn list_names(
        &self,
        maximum_entries: usize,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<Vec<String>> {
        #[cfg(unix)]
        return list::list_names(&self.handle, maximum_entries, cancelled);
        #[cfg(not(unix))]
        {
            let mut names = Vec::new();
            for entry in fs::read_dir(self.path.as_path())
                .context("failed to scan managed workflow directory")?
            {
                if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
                    bail!("managed workflow directory scan was cancelled");
                }
                let entry = entry.context("failed to inspect managed workflow directory entry")?;
                let name = entry.file_name();
                let name = name
                    .to_str()
                    .context("managed workflow directory entry must be UTF-8")?;
                validate_component(name)?;
                names.push(name.to_owned());
                if names.len() > maximum_entries {
                    bail!("managed workflow directory scan exceeds its entry limit");
                }
            }
            names.sort();
            Ok(names)
        }
    }

    #[cfg(unix)]
    pub(super) fn device_id(&self) -> anyhow::Result<rustix::fs::Dev> {
        Ok(rustix::fs::fstat(&self.handle)?.st_dev)
    }

    #[cfg(windows)]
    pub(super) fn device_id(&self) -> anyhow::Result<u64> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
        use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

        let raw = self.guard.handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to inspect managed volume");
        }
        Ok(u64::from(info.dwVolumeSerialNumber))
    }

    /// Opens or creates a permanent, owner-private advisory lock file.
    pub(super) fn open_lock_file(&self, name: &str) -> anyhow::Result<fs::File> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::openat;
            use rustix::io::Errno;

            let exclusive =
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let (handle, created) =
                match openat(&self.handle, name, exclusive, Mode::RUSR | Mode::WUSR) {
                    Ok(handle) => (handle, true),
                    Err(Errno::EXIST) => (
                        openat(
                            &self.handle,
                            name,
                            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .context("failed to open existing managed lock file")?,
                        false,
                    ),
                    Err(error) => return Err(error).context("failed to create managed lock file"),
                };
            let metadata = rustix::fs::fstat(&handle)?;
            if metadata.st_mode & 0o170000 != 0o100000 || metadata.st_mode & 0o077 != 0 {
                bail!("managed lock must be an owner-private regular file");
            }
            if created {
                self.sync()?;
            }
            Ok(fs::File::from(handle))
        }
        #[cfg(windows)]
        {
            let path = self.path.join(name);
            match super::windows_security::create_private_file(path.as_path()) {
                Ok(file) => Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    super::windows_security::open_private_file(path.as_path())
                        .context("failed to open private managed lock file")
                }
                Err(error) => Err(error).context("failed to create private managed lock file"),
            }
        }
        #[cfg(not(any(unix, windows)))]
        bail!("managed lock files are unsupported on this platform");
    }

    /// Opens an existing private child without creating metadata during reads.
    pub(super) fn existing_child(&self, name: &str) -> anyhow::Result<Self> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::openat;

            let handle = openat(
                &self.handle,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .context("failed to open existing managed workflow directory")?;
            if rustix::fs::fstat(&handle)?.st_mode & 0o077 != 0 {
                bail!("managed workflow directory is accessible to other users");
            }
            Ok(Self {
                path: self.path.join(name),
                handle,
            })
        }
        #[cfg(windows)]
        {
            let path = self.path.join(name);
            let handle = super::windows_security::open_directory(
                path.as_path(),
                /*private*/ true,
                /*desired_access*/ 0,
            )?;
            Ok(Self {
                path,
                guard: std::sync::Arc::new(WindowsDirectoryGuard {
                    handle,
                    _parent: Some(std::sync::Arc::clone(&self.guard)),
                }),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.path.join(name);
            let metadata = fs::symlink_metadata(path.as_path())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("managed workflow directory must not be aliased");
            }
            Ok(Self { path })
        }
    }

    /// Inspects a child without creating it, for pre-mutation volume checks.
    #[cfg(any(unix, windows))]
    pub(super) fn optional_existing_child(&self, name: &str) -> anyhow::Result<Option<Self>> {
        #[cfg(windows)]
        {
            validate_component(name)?;
            let path = self.path.join(name);
            let handle = match super::windows_security::open_directory(
                path.as_path(),
                /*private*/ true,
                /*desired_access*/ 0,
            ) {
                Ok(handle) => handle,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error).context("failed to inspect managed directory"),
            };
            return Ok(Some(Self {
                path,
                guard: std::sync::Arc::new(WindowsDirectoryGuard {
                    handle,
                    _parent: Some(std::sync::Arc::clone(&self.guard)),
                }),
            }));
        }
        #[cfg(unix)]
        {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::openat;
            use rustix::io::Errno;

            validate_component(name)?;
            let handle = match openat(
                &self.handle,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(handle) => handle,
                Err(Errno::NOENT) => return Ok(None),
                Err(error) => {
                    return Err(error).context("failed to inspect managed workflow directory");
                }
            };
            if rustix::fs::fstat(&handle)?.st_mode & 0o077 != 0 {
                bail!("managed workflow directory is accessible to other users");
            }
            Ok(Some(Self {
                path: self.path.join(name),
                handle,
            }))
        }
    }

    /// Opens an existing regular metadata file without following an alias.
    pub(super) fn read_file(&self, name: &str, maximum_bytes: u64) -> anyhow::Result<Vec<u8>> {
        validate_component(name)?;
        let mut file = self.open_regular_file(name)?;
        let metadata = file.metadata()?;
        if metadata.len() > maximum_bytes {
            bail!("managed workflow metadata exceeds {maximum_bytes} bytes");
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum_bytes + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > maximum_bytes {
            bail!("managed workflow metadata exceeds {maximum_bytes} bytes");
        }
        Ok(bytes)
    }

    /// Publishes a complete file and syncs both content and containing directory.
    pub(super) fn write_file(&self, name: &str, bytes: &[u8], replace: bool) -> anyhow::Result<()> {
        validate_component(name)?;
        #[cfg(unix)]
        {
            use std::sync::atomic::AtomicU64;
            use std::sync::atomic::Ordering;

            use rustix::fs::AtFlags;
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::RenameFlags;
            use rustix::fs::openat;
            use rustix::fs::renameat_with;
            use rustix::fs::statat;
            use rustix::fs::unlinkat;
            use rustix::io::Errno;

            static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);
            match statat(&self.handle, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) if replace && metadata.st_mode & 0o170000 == 0o100000 => {}
                Ok(_) if replace => {
                    bail!("managed workflow metadata target must be a regular file")
                }
                Ok(_) => bail!("managed workflow metadata already exists"),
                Err(Errno::NOENT) if !replace => {}
                Err(Errno::NOENT) => bail!("managed workflow metadata target is missing"),
                Err(error) => {
                    return Err(error).context("failed to inspect managed metadata target");
                }
            }
            for _ in 0..128 {
                let sequence = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
                let temporary_name = format!(".managed-file-{}-{sequence}", std::process::id());
                let opened = openat(
                    &self.handle,
                    temporary_name.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                );
                let mut file = match opened {
                    Ok(file) => fs::File::from(file),
                    Err(Errno::EXIST) => continue,
                    Err(error) => return Err(error).context("failed to stage managed metadata"),
                };
                let result = file.write_all(bytes).and_then(|()| file.sync_all());
                drop(file);
                if let Err(error) = result {
                    let _ = unlinkat(&self.handle, temporary_name.as_str(), AtFlags::empty());
                    return Err(error).context("failed to write managed metadata");
                }
                let flags = if replace {
                    RenameFlags::empty()
                } else {
                    RenameFlags::NOREPLACE
                };
                if let Err(error) = renameat_with(
                    &self.handle,
                    temporary_name.as_str(),
                    &self.handle,
                    name,
                    flags,
                ) {
                    let _ = unlinkat(&self.handle, temporary_name.as_str(), AtFlags::empty());
                    return Err(error).context("failed to publish managed metadata");
                }
                self.sync()?;
                return Ok(());
            }
            bail!("failed to allocate a unique managed metadata file");
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use std::sync::atomic::AtomicU64;
            use std::sync::atomic::Ordering;
            use windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING;
            use windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH;
            use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

            let target = self.path.join(name);
            if replace {
                self.open_regular_file(name)?;
            } else if fs::symlink_metadata(target.as_path()).is_ok() {
                bail!("managed workflow metadata already exists");
            }
            let target_wide = target
                .as_path()
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let flags = MOVEFILE_WRITE_THROUGH
                | if replace {
                    MOVEFILE_REPLACE_EXISTING
                } else {
                    0
                };
            static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(0);
            for _ in 0..128 {
                let sequence = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
                let temporary = self
                    .path
                    .join(format!(".managed-file-{}-{sequence}", std::process::id()));
                let mut file =
                    match super::windows_security::create_private_file(temporary.as_path()) {
                        Ok(file) => file,
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(error) => {
                            return Err(error).context("failed to stage private managed metadata");
                        }
                    };
                let written = file.write_all(bytes).and_then(|()| file.sync_all());
                drop(file);
                if let Err(error) = written {
                    let _ = fs::remove_file(temporary.as_path());
                    return Err(error).context("failed to write private managed metadata");
                }
                let source_wide = temporary
                    .as_path()
                    .as_os_str()
                    .encode_wide()
                    .chain(Some(0))
                    .collect::<Vec<_>>();
                if unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), flags) } == 0 {
                    let error = std::io::Error::last_os_error();
                    let _ = fs::remove_file(temporary.as_path());
                    return Err(error).context("failed to publish managed metadata");
                }
                return Ok(());
            }
            bail!("failed to allocate a unique private managed metadata file");
        }
        #[cfg(not(any(unix, windows)))]
        {
            if replace {
                self.open_regular_file(name)?;
            } else if self.path.join(name).as_path().exists() {
                bail!("managed workflow metadata already exists");
            }
            let mut temporary = tempfile::Builder::new()
                .prefix(".managed-file-")
                .tempfile_in(self.path.as_path())
                .context("failed to stage managed workflow metadata")?;
            temporary.write_all(bytes)?;
            temporary.as_file().sync_all()?;
            let target = self.path.join(name);
            if replace {
                temporary
                    .persist(target.as_path())
                    .map_err(|error| error.error)?;
            } else {
                temporary
                    .persist_noclobber(target.as_path())
                    .map_err(|error| error.error)?;
            }
            self.sync()?;
            Ok(())
        }
    }

    pub(super) fn sync(&self) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            use rustix::fs::fsync;
            fsync(&self.handle).context("failed to sync managed workflow directory")?;
        }
        // Windows file contents are flushed through their file handles, and
        // metadata renames use MOVEFILE_WRITE_THROUGH or FileRenameInfo.
        Ok(())
    }

    pub(super) fn rename_child_noreplace(
        &self,
        source_name: &str,
        target_parent: &Self,
        target_name: &str,
    ) -> anyhow::Result<()> {
        validate_component(source_name)?;
        validate_component(target_name)?;
        #[cfg(unix)]
        {
            use rustix::fs::AtFlags;
            use rustix::fs::RenameFlags;
            use rustix::fs::renameat_with;
            use rustix::fs::statat;

            let source = statat(&self.handle, source_name, AtFlags::SYMLINK_NOFOLLOW)
                .context("failed to inspect managed workflow directory before rename")?;
            if source.st_mode & 0o170000 != 0o040000 {
                bail!("managed workflow source must be a regular directory");
            }
            renameat_with(
                &self.handle,
                source_name,
                &target_parent.handle,
                target_name,
                RenameFlags::NOREPLACE,
            )
            .context("failed to publish managed workflow directory")?;
            self.sync()?;
            target_parent.sync()?;
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::DELETE;
            use windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO;
            use windows_sys::Win32::Storage::FileSystem::FILE_RENAME_INFO_0;
            use windows_sys::Win32::Storage::FileSystem::FileRenameInfo;
            use windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle;

            let source = super::windows_security::open_directory(
                self.path.join(source_name).as_path(),
                /*private*/ true,
                /*desired_access*/ DELETE,
            )?;
            // The validated target parent stays pinned while Windows resolves
            // the absolute name. RootDirectory plus a relative name is not
            // accepted by all supported Windows builds.
            let target_path = target_parent.path.join(target_name);
            let target_name = target_path
                .as_path()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>();
            let filename_bytes = target_name
                .len()
                .checked_mul(std::mem::size_of::<u16>())
                .context("managed rename name length overflow")?;
            let filename_size = u32::try_from(filename_bytes)?;
            let information_bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName)
                .checked_add(filename_bytes)
                .and_then(|size| size.checked_add(std::mem::size_of::<u16>()))
                .context("managed rename information length overflow")?;
            let information_size = u32::try_from(information_bytes)?;
            let mut storage =
                vec![0_usize; information_bytes.div_ceil(std::mem::size_of::<usize>())];
            let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
            unsafe {
                std::ptr::addr_of_mut!((*information).Anonymous)
                    .write(FILE_RENAME_INFO_0 { ReplaceIfExists: 0 });
                std::ptr::addr_of_mut!((*information).RootDirectory).write(0);
                std::ptr::addr_of_mut!((*information).FileNameLength).write(filename_size);
                target_name.as_ptr().copy_to_nonoverlapping(
                    std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
                    target_name.len(),
                );
            }
            if unsafe {
                SetFileInformationByHandle(
                    source.as_raw_handle() as _,
                    FileRenameInfo,
                    information.cast(),
                    information_size,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to rename managed workflow directory");
            }
            self.sync()?;
            target_parent.sync()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let source = self.path.join(source_name);
            let target = target_parent.path.join(target_name);
            if fs::symlink_metadata(target.as_path()).is_ok() {
                bail!("managed workflow target already exists");
            }
            fs::rename(source.as_path(), target.as_path())
                .context("failed to publish managed workflow directory")?;
            Ok(())
        }
    }

    #[cfg(unix)]
    pub(super) fn remove_regular_file(&self, name: &str) -> anyhow::Result<()> {
        use rustix::fs::AtFlags;
        use rustix::fs::statat;
        use rustix::fs::unlinkat;

        validate_component(name)?;
        let metadata = statat(&self.handle, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if metadata.st_mode & 0o170000 != 0o100000 {
            bail!("managed metadata target must be a regular file");
        }
        unlinkat(&self.handle, name, AtFlags::empty())?;
        self.sync()
    }

    fn open_regular_file(&self, name: &str) -> anyhow::Result<fs::File> {
        #[cfg(unix)]
        let file = {
            use rustix::fs::Mode;
            use rustix::fs::OFlags;
            use rustix::fs::openat;
            fs::File::from(openat(
                &self.handle,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?)
        };
        #[cfg(windows)]
        let file = super::windows_security::open_private_file(self.path.join(name).as_path())?;
        #[cfg(not(any(unix, windows)))]
        let file = {
            let path = self.path.join(name);
            let mut options = fs::OpenOptions::new();
            options.read(true);
            options.open(path.as_path())?
        };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || is_windows_reparse_point(&metadata)
        {
            bail!("managed workflow metadata must be a regular file without aliases");
        }
        Ok(file)
    }
}

fn validate_component(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
        bail!("invalid managed workflow path component");
    }
    super::super::fetch::portable_path(name)?;
    Ok(())
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(all(test, windows))]
#[path = "fs/windows_tests.rs"]
mod windows_tests;
