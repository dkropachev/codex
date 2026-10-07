use std::fs;
use std::io::Read;
use std::io::Write;
use std::os::fd::OwnedFd;

use anyhow::Context;
use anyhow::bail;
use rustix::fs::AtFlags;
use rustix::fs::Mode;
use rustix::fs::OFlags;
use rustix::fs::openat;
use rustix::fs::statat;
use rustix::fs::unlinkat;

use super::fs::SecureDirectory;
use super::fs::device_id_from_stat;
use super::fs::list::list_raw_names;

// Activation payload limit plus transaction-owned directories and markers.
pub(super) struct CleanupEntryLimit(pub(super) usize);

impl CleanupEntryLimit {
    pub(super) const STANDARD: Self = Self(250_016);
}

#[derive(Clone, Copy)]
pub(super) enum OwnershipMarker<'a> {
    Required,
    CreationIncomplete(&'a OwnedFd),
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
    directory: OwnedFd,
    name: String,
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
    let record_name = ownership_record_name(name);
    let metadata = match statat(parent.handle(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => {
            if let Some((record_device, record_inode, _)) =
                read_ownership_record(parent, &record_name, name)?
            {
                ensure_named_identity(parent.handle(), &record_name, record_device, record_inode)?;
                unlinkat(parent.handle(), record_name.as_str(), AtFlags::empty())?;
                parent.sync()?;
            }
            return Ok(());
        }
        Err(error) => return Err(error).context("failed to inspect transaction staging cleanup"),
    };
    let (record_device, record_inode, recorded_root) =
        read_ownership_record(parent, &record_name, name)?
            .context("transaction staging has no sibling ownership record")?;
    match recorded_root {
        RecordedRoot::Bound {
            device: recorded_device,
            inode: recorded_inode,
        } if recorded_device == device && recorded_inode == inode => {}
        RecordedRoot::Reservation
            if matches!(ownership, OwnershipMarker::CreationIncomplete(_)) =>
        {
            if let OwnershipMarker::CreationIncomplete(original) = ownership {
                let opened = rustix::fs::fstat(original)?;
                if device_id_from_stat(opened.st_dev) != device || opened.st_ino != inode {
                    bail!("transaction creation proof does not match directory identity");
                }
            }
        }
        RecordedRoot::Bound { .. } | RecordedRoot::Reservation => {
            bail!("transaction ownership record does not match directory identity");
        }
    }
    if metadata.st_mode & 0o170000 != 0o040000
        || device_id_from_stat(metadata.st_dev) != device
        || metadata.st_ino != inode
    {
        bail!("workflow transaction staging changed identity before cleanup");
    }
    let root = openat(
        parent.handle(),
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let opened = rustix::fs::fstat(&root)?;
    if device_id_from_stat(opened.st_dev) != device || opened.st_ino != inode {
        bail!("workflow transaction staging changed identity while opening cleanup root");
    }
    if matches!(ownership, OwnershipMarker::Required) {
        let marker = match openat(
            &root,
            ".codex-managed-operation",
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(marker) => Some(marker),
            Err(rustix::io::Errno::NOENT) => None,
            Err(error) => return Err(error).context("failed to inspect transaction marker"),
        };
        if let Some(marker) = marker {
            if rustix::fs::fstat(&marker)?.st_mode & 0o170000 != 0o100000 {
                bail!("transaction staging ownership marker is not a regular file");
            }
            let mut marker_bytes = Vec::new();
            fs::File::from(marker)
                .take(129)
                .read_to_end(&mut marker_bytes)?;
            if marker_bytes != name.as_bytes() {
                bail!("transaction staging ownership marker does not match directory");
            }
        }
    }
    let mut root_names = list_raw_names(&root, maximum_entries.0, /*cancelled*/ None)?;
    let mut entries = 0_usize;
    if matches!(ownership, OwnershipMarker::Required)
        && root_names
            .iter()
            .any(|entry| entry == ".codex-managed-operation")
    {
        root_names.retain(|entry| entry != ".codex-managed-operation");
        entries = 1;
    }
    let mut stack = vec![Frame {
        remaining: root_names,
        directory: root,
        name: name.to_owned(),
        device,
        inode,
    }];
    while let Some(frame) = stack.last_mut() {
        if let Some(child_name) = frame.remaining.pop() {
            entries = entries
                .checked_add(1)
                .context("transaction cleanup entry count overflow")?;
            if entries > maximum_entries.0 {
                bail!("transaction cleanup exceeds its entry limit");
            }
            let child = statat(
                &frame.directory,
                child_name.as_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            )?;
            if child.st_mode & 0o170000 == 0o040000 {
                let directory = openat(
                    &frame.directory,
                    child_name.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                let opened = rustix::fs::fstat(&directory)?;
                if opened.st_dev != child.st_dev || opened.st_ino != child.st_ino {
                    bail!("transaction cleanup child changed identity while opening");
                }
                let remaining = list_raw_names(
                    &directory,
                    maximum_entries.0 - entries,
                    /*cancelled*/ None,
                )?;
                stack.push(Frame {
                    directory,
                    name: child_name,
                    device: device_id_from_stat(child.st_dev),
                    inode: child.st_ino,
                    remaining,
                });
            } else {
                ensure_named_identity(
                    &frame.directory,
                    &child_name,
                    device_id_from_stat(child.st_dev),
                    child.st_ino,
                )?;
                unlinkat(&frame.directory, child_name.as_str(), AtFlags::empty())?;
            }
        } else {
            let finished = stack.pop().context("transaction cleanup stack was empty")?;
            if let Some(ancestor) = stack.last() {
                ensure_named_identity(
                    &ancestor.directory,
                    &finished.name,
                    finished.device,
                    finished.inode,
                )?;
                unlinkat(
                    &ancestor.directory,
                    finished.name.as_str(),
                    AtFlags::REMOVEDIR,
                )?;
            } else {
                ensure_named_identity(parent.handle(), &finished.name, device, inode)?;
                if matches!(ownership, OwnershipMarker::Required)
                    && statat(
                        &finished.directory,
                        ".codex-managed-operation",
                        AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .is_ok()
                {
                    unlinkat(
                        &finished.directory,
                        ".codex-managed-operation",
                        AtFlags::empty(),
                    )?;
                }
                if let Err(error) =
                    unlinkat(parent.handle(), finished.name.as_str(), AtFlags::REMOVEDIR)
                {
                    if matches!(ownership, OwnershipMarker::Required) {
                        let _ = restore_marker(&finished.directory, name);
                    }
                    return Err(error).context("failed to remove transaction staging root");
                }
                parent.sync()?;
                ensure_named_identity(parent.handle(), &record_name, record_device, record_inode)?;
                unlinkat(parent.handle(), record_name.as_str(), AtFlags::empty())?;
                parent.sync()?;
            }
        }
    }
    Ok(())
}

fn read_ownership_record(
    parent: &SecureDirectory,
    record_name: &str,
    expected_name: &str,
) -> anyhow::Result<Option<(u64, u64, RecordedRoot)>> {
    let record = match openat(
        parent.handle(),
        record_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(record) => record,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error).context("failed to open transaction ownership record"),
    };
    let metadata = rustix::fs::fstat(&record)?;
    if metadata.st_mode & 0o170000 != 0o100000 {
        bail!("transaction ownership record is not a regular file");
    }
    let mut contents = Vec::new();
    fs::File::from(record)
        .take(257)
        .read_to_end(&mut contents)?;
    let text = std::str::from_utf8(&contents).context("invalid transaction ownership record")?;
    let mut fields = text.lines();
    let recorded_root = match (
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
    };
    Ok(Some((
        device_id_from_stat(metadata.st_dev),
        metadata.st_ino,
        recorded_root,
    )))
}

fn restore_marker(directory: &OwnedFd, name: &str) -> anyhow::Result<()> {
    let marker = openat(
        directory,
        ".codex-managed-operation",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut marker = fs::File::from(marker);
    marker.write_all(name.as_bytes())?;
    marker.sync_all()?;
    rustix::fs::fsync(directory)?;
    Ok(())
}

fn ensure_named_identity(
    parent: &OwnedFd,
    name: &str,
    device: u64,
    inode: u64,
) -> anyhow::Result<()> {
    let named = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if device_id_from_stat(named.st_dev) != device || named.st_ino != inode {
        bail!("transaction cleanup entry changed identity");
    }
    Ok(())
}

#[cfg(all(test, unix))]
#[path = "cleanup_tests.rs"]
mod tests;
