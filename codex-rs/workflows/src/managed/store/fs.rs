use std::fs;
#[cfg(not(windows))]
use std::io::Read;
#[cfg(not(windows))]
use std::io::Write;
use std::sync::atomic::AtomicBool;
#[cfg(not(unix))]
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

pub(super) mod list;

/// A retained, non-aliased directory used as the parent of managed metadata.
pub(super) struct SecureDirectory {
    path: AbsolutePathBuf,
    #[cfg(unix)]
    handle: std::os::fd::OwnedFd,
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
            let _ = path;
            bail!("Windows managed store requires protected ACL and handle-relative support");
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
            let _ = path;
            bail!("Windows managed store directories require protected ACL support");
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
    pub(super) fn device_id(&self) -> anyhow::Result<u64> {
        Ok(rustix::fs::fstat(&self.handle)?.st_dev)
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
        bail!("Windows managed locks require protected ACL and handle-relative support");
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
        bail!("Windows managed store directories require protected ACL support");
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
    #[cfg(unix)]
    pub(super) fn optional_existing_child(&self, name: &str) -> anyhow::Result<Option<Self>> {
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

    /// Opens an existing regular metadata file without following an alias.
    pub(super) fn read_file(&self, name: &str, maximum_bytes: u64) -> anyhow::Result<Vec<u8>> {
        #[cfg(windows)]
        {
            let _ = (name, maximum_bytes);
            bail!("Windows managed store file access requires handle-relative support");
        }
        #[cfg(not(windows))]
        {
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
            let _ = (bytes, replace);
            bail!("Windows managed store writes require protected ACL and handle-relative support");
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
        #[cfg(windows)]
        bail!("Windows managed store durability support is not yet available");
        #[cfg(not(windows))]
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
            let _ = target_parent;
            bail!("Windows managed store rename requires atomic handle-relative support");
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

    #[cfg(not(windows))]
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
        #[cfg(not(unix))]
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
