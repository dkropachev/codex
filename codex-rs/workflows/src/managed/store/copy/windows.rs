use std::fs;
use std::io::Read;
use std::io::Write;
use std::os::windows::fs::MetadataExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;

use super::super::fs::SecureDirectory;
use crate::managed::fetch::VerificationLimits;

struct Pending {
    source: SecureDirectory,
    destination: SecureDirectory,
    relative: PathBuf,
}

pub(super) fn copy_verified_payload(
    source_root: &AbsolutePathBuf,
    destination: &SecureDirectory,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    deadline.check(cancelled)?;
    let root = SecureDirectory::open_root(source_root)?;
    let mut pending = vec![Pending {
        source: root,
        destination: destination.child("payload")?,
        relative: PathBuf::new(),
    }];
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    while let Some(Pending {
        source,
        destination,
        relative,
    }) = pending.pop()
    {
        deadline.check(cancelled)?;
        let remaining = limits
            .post_install_entries
            .checked_sub(entries)
            .context("workflow copy entry count overflow")?;
        let enumeration_limit = if relative.as_os_str().is_empty() {
            remaining
                .checked_add(1)
                .context("workflow copy entry count overflow")?
        } else {
            remaining
        };
        for name in source.list_raw_names(enumeration_limit, cancelled)? {
            deadline.check(cancelled)?;
            if relative.as_os_str().is_empty() && name == ".git" {
                continue;
            }
            entries = entries
                .checked_add(1)
                .context("workflow copy entry count overflow")?;
            if entries > limits.post_install_entries {
                bail!("workflow copy exceeds its entry limit");
            }
            let relative_path = relative.join(&name);
            let portable = relative_path
                .to_str()
                .context("workflow copy path must be UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            crate::managed::fetch::portable_path(&portable)?;
            let source_path = source.path().join(&name);
            let metadata = fs::symlink_metadata(source_path.as_path())?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                if !metadata.file_type().is_symlink()
                    || !relative_path.starts_with("node_modules")
                    || relative_path == Path::new("node_modules")
                {
                    bail!("verified workflow payload contains an unsupported reparse point");
                }
                let target = fs::read_link(source_path.as_path())?;
                crate::managed::integrity::validate_dependency_link(
                    source_root.as_path().join("node_modules").as_path(),
                    source_path.as_path(),
                    &target,
                )?;
                let target_bytes = target
                    .to_str()
                    .context("workflow dependency link target must be UTF-8")?
                    .len() as u64;
                bytes = bytes
                    .checked_add(target_bytes)
                    .context("workflow copy byte count overflow")?;
                if bytes > limits.post_install_bytes {
                    bail!("workflow copy exceeds its logical byte limit");
                }
                let destination_path = destination.path().join(&name);
                if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    std::os::windows::fs::symlink_dir(&target, destination_path.as_path())?;
                } else {
                    std::os::windows::fs::symlink_file(&target, destination_path.as_path())?;
                }
            } else if metadata.is_dir() {
                let source_child = SecureDirectory::open_root(&source_path)?;
                let destination_child = destination.child(&name)?;
                pending.push(Pending {
                    source: source_child,
                    destination: destination_child,
                    relative: relative_path,
                });
            } else if metadata.is_file() {
                copy_file(
                    source_path.as_path(),
                    destination.path().join(&name).as_path(),
                    &mut bytes,
                    limits.post_install_bytes,
                    deadline,
                    cancelled,
                )?;
            } else {
                bail!("verified workflow payload contains an unsupported file type");
            }
        }
        destination.sync()?;
    }
    Ok(())
}

fn copy_file(
    source: &Path,
    target: &Path,
    bytes: &mut u64,
    maximum_bytes: u64,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut source = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)?;
    let metadata = source.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("verified workflow source file changed type during copy");
    }
    let mut target = super::super::windows_security::create_private_file(target)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        deadline.check(cancelled)?;
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        *bytes = bytes
            .checked_add(read as u64)
            .context("workflow copy byte count overflow")?;
        if *bytes > maximum_bytes {
            bail!("workflow copy exceeds its logical byte limit");
        }
        target.write_all(&buffer[..read])?;
    }
    target.sync_all()?;
    Ok(())
}
