use std::fs;
use std::io::Read;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;
use rustix::fs::AtFlags;
use rustix::fs::Mode;
use rustix::fs::OFlags;
use rustix::fs::open;
use rustix::fs::openat;
use rustix::fs::statat;

use super::super::fs::SecureDirectory;
use super::super::fs::list::list_raw_names;
use crate::managed::fetch::VerificationLimits;

struct Pending {
    source: OwnedFd,
    destination: SecureDirectory,
    relative: PathBuf,
}

pub(super) fn copy_verified_payload(
    source: &AbsolutePathBuf,
    destination: &SecureDirectory,
    limits: VerificationLimits,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    deadline.check(cancelled)?;
    let root = open(
        source.as_path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .context("failed to open verified workflow checkout")?;
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
        for name in list_raw_names(&source, enumeration_limit, cancelled)? {
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
            let path = relative.join(&name);
            let portable = path
                .to_str()
                .context("workflow copy path must be UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            crate::managed::fetch::portable_path(&portable)?;
            let metadata = statat(&source, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)?;
            match metadata.st_mode & 0o170000 {
                0o040000 => {
                    let child_source = openat(
                        &source,
                        name.as_str(),
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?;
                    let child_destination = destination.child(&name)?;
                    pending.push(Pending {
                        source: child_source,
                        destination: child_destination,
                        relative: path,
                    });
                }
                0o100000 => copy_file(
                    &source,
                    name.as_str(),
                    &destination,
                    &mut bytes,
                    limits.post_install_bytes,
                    deadline,
                    cancelled,
                )?,
                0o120000
                    if path.starts_with("node_modules")
                        && path != PathBuf::from("node_modules") =>
                {
                    let target = rustix::fs::readlinkat(&source, name.as_str(), Vec::new())?;
                    if target.to_bytes().len() > 4096 {
                        bail!("workflow dependency link target is oversized");
                    }
                    bytes = bytes
                        .checked_add(target.to_bytes().len() as u64)
                        .context("workflow copy byte count overflow")?;
                    if bytes > limits.post_install_bytes {
                        bail!("workflow copy exceeds its logical byte limit");
                    }
                    rustix::fs::symlinkat(target.as_c_str(), destination.handle(), name.as_str())?;
                    destination.sync()?;
                }
                _ => bail!("verified workflow payload contains an unsupported file type"),
            }
        }
        destination.sync()?;
    }
    Ok(())
}

fn copy_file(
    source_dir: &OwnedFd,
    name: &str,
    destination_dir: &SecureDirectory,
    total_bytes: &mut u64,
    maximum_bytes: u64,
    deadline: crate::runner::CommandDeadline,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let source = openat(
        source_dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let source_mode = rustix::fs::fstat(&source)?.st_mode;
    if source_mode & 0o170000 != 0o100000 {
        bail!("verified workflow file changed type during copy");
    }
    let target = openat(
        destination_dir.handle(),
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut source = fs::File::from(source);
    let mut target = fs::File::from(target);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        deadline.check(cancelled)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        *total_bytes = total_bytes
            .checked_add(count as u64)
            .context("workflow copy byte count overflow")?;
        if *total_bytes > maximum_bytes {
            bail!("workflow copy exceeds its logical byte limit");
        }
        target.write_all(&buffer[..count])?;
    }
    let mode = if source_mode & 0o111 == 0 {
        Mode::RUSR | Mode::WUSR
    } else {
        Mode::RUSR | Mode::WUSR | Mode::XUSR
    };
    rustix::fs::fchmod(&target, mode)?;
    target.sync_all()?;
    destination_dir.sync()?;
    Ok(())
}
