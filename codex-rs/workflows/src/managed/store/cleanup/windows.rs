#![allow(
    dead_code,
    reason = "used by Windows transaction staging in the next stage"
)]

use std::fs;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::bail;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

use super::fs::SecureDirectory;

pub(super) struct CleanupEntryLimit(pub(super) usize);

impl CleanupEntryLimit {
    pub(super) const STANDARD: Self = Self(250_016);
}

#[derive(Clone, Copy)]
pub(super) enum OwnershipMarker<'a> {
    Required,
    CreationIncomplete(&'a SecureDirectory),
}

pub(super) fn ownership_record_name(name: &str) -> String {
    format!(".owner-{name}")
}

pub(super) fn reservation_record(name: &str) -> Vec<u8> {
    format!("v1\n{name}\n").into_bytes()
}

pub(super) fn bound_record(name: &str, device: u64, inode: u64) -> Vec<u8> {
    format!("v1\n{name}\n{device}\n{inode}\n").into_bytes()
}

enum RecordedRoot {
    Reservation,
    Bound { device: u64, inode: u64 },
}

struct Frame {
    directory: SecureDirectory,
    path: PathBuf,
    device: u64,
    inode: u64,
    remaining: Vec<String>,
}

pub(super) fn remove_tree(
    parent: &SecureDirectory,
    name: &str,
    device: u64,
    inode: u64,
    ownership: OwnershipMarker<'_>,
    maximum_entries: CleanupEntryLimit,
) -> anyhow::Result<()> {
    crate::managed::fetch::portable_path(name)?;
    let record_name = ownership_record_name(name);
    let record_path = parent.path().join(&record_name);
    let record = match fs::symlink_metadata(record_path.as_path()) {
        Ok(_) => Some(read_record(parent, &record_name, name)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("failed to inspect transaction ownership record"),
    };
    let Some(root) = parent.optional_existing_child(name)? else {
        if record.is_some() {
            parent.remove_regular_file(&record_name)?;
        }
        return Ok(());
    };
    let recorded = record.context("transaction staging has no sibling ownership record")?;
    match recorded {
        RecordedRoot::Bound {
            device: recorded_device,
            inode: recorded_inode,
        } if recorded_device == device && recorded_inode == inode => {}
        RecordedRoot::Reservation => {
            if let OwnershipMarker::CreationIncomplete(original) = ownership {
                if original.identity()? != (device, inode) {
                    bail!("transaction creation proof does not match directory identity");
                }
            } else {
                bail!("transaction ownership reservation cannot authorize recovery cleanup");
            }
        }
        RecordedRoot::Bound { .. } => {
            bail!("transaction ownership record does not match directory identity")
        }
    }
    if root.identity()? != (device, inode) {
        bail!("transaction staging changed identity before cleanup");
    }
    match fs::symlink_metadata(root.path().join(".codex-managed-operation").as_path()) {
        Ok(_) => {
            if root.read_file(".codex-managed-operation", 128)? != name.as_bytes() {
                bail!("transaction staging operation marker does not match directory");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to inspect transaction marker"),
    }
    let mut entries = 0_usize;
    let names = root.list_raw_names(maximum_entries.0, /*cancelled*/ None)?;
    let mut stack = vec![Frame {
        path: root.path().as_path().to_path_buf(),
        device,
        inode,
        remaining: names,
        directory: root,
    }];
    while let Some(frame) = stack.last_mut() {
        if let Some(child_name) = frame.remaining.pop() {
            entries = entries
                .checked_add(1)
                .context("transaction cleanup entry count overflow")?;
            if entries > maximum_entries.0 {
                bail!("transaction cleanup exceeds its entry limit");
            }
            let child_path = frame.path.join(&child_name);
            let metadata = fs::symlink_metadata(&child_path)?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                if !metadata.file_type().is_symlink() {
                    bail!("transaction cleanup contains an unknown reparse point");
                }
                if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
                    fs::remove_dir(&child_path)?;
                } else {
                    fs::remove_file(&child_path)?;
                }
            } else if metadata.is_dir() {
                let directory = frame.directory.existing_child(&child_name)?;
                let (child_device, child_inode) = directory.identity()?;
                let remaining = directory
                    .list_raw_names(maximum_entries.0 - entries, /*cancelled*/ None)?;
                stack.push(Frame {
                    path: child_path,
                    device: child_device,
                    inode: child_inode,
                    remaining,
                    directory,
                });
            } else if metadata.is_file() {
                fs::remove_file(&child_path)?;
            } else {
                bail!("transaction cleanup contains a special file");
            }
        } else {
            let finished = stack.pop().context("transaction cleanup stack was empty")?;
            let Frame {
                directory,
                path,
                device,
                inode,
                ..
            } = finished;
            drop(directory);
            ensure_named_identity(&path, device, inode)?;
            fs::remove_dir(&path)?;
        }
    }
    parent.sync()?;
    parent.remove_regular_file(&record_name)
}

fn read_record(
    parent: &SecureDirectory,
    record_name: &str,
    expected_name: &str,
) -> anyhow::Result<RecordedRoot> {
    let bytes = parent.read_file(record_name, 256)?;
    let text = std::str::from_utf8(&bytes).context("invalid transaction ownership record")?;
    let mut fields = text.lines();
    Ok(
        match (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) {
            (Some("v1"), Some(name), None, None, None) if name == expected_name => {
                RecordedRoot::Reservation
            }
            (Some("v1"), Some(name), Some(device), Some(inode), None) if name == expected_name => {
                RecordedRoot::Bound {
                    device: device.parse().context("invalid ownership record device")?,
                    inode: inode.parse().context("invalid ownership record inode")?,
                }
            }
            _ => bail!("transaction ownership record does not match directory"),
        },
    )
}

fn ensure_named_identity(path: &Path, device: u64, inode: u64) -> anyhow::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
    use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;

    let handle = super::windows_security::open_directory(
        path, /*private*/ true, /*desired_access*/ 0,
    )?;
    let raw = handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("failed to inspect cleanup entry");
    }
    if u64::from(info.dwVolumeSerialNumber) != device
        || ((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)) != inode
    {
        bail!("transaction cleanup entry changed identity");
    }
    Ok(())
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
