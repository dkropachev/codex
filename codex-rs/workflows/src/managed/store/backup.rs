use anyhow::Context;
use anyhow::bail;
use rustix::fs::AtFlags;
use rustix::fs::statat;
use rustix::io::Errno;

use super::ManagedWorkflowStore;
use super::cleanup;
use super::fs::SecureDirectory;
use super::fs::device_id_from_stat;
use super::journal::ManagedWorkflowJournal;
use super::journal::read_marker;
use crate::managed::fetch::VERIFICATION_LIMITS;
use crate::managed::integrity::backup_payload_evidence;
use crate::managed::integrity::published_payload_evidence;

/// Moves the old marked release to a journal-owned backup and binds its identity.
pub(super) fn move_current_aside(
    store: &ManagedWorkflowStore,
    journal: &ManagedWorkflowJournal,
    active_parent: &SecureDirectory,
    active_name: &str,
) -> anyhow::Result<()> {
    let name = journal.transaction_id.as_str();
    let record_name = cleanup::ownership_record_name(name);
    let backup = if let Some(backup) = store.backups.optional_existing_child(name)? {
        backup
    } else {
        let active = active_parent.existing_child(active_name)?;
        verify_old_active(&active, journal)?;
        drop(active);
        match statat(
            store.backups.handle(),
            record_name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(_) => {
                if store
                    .backups
                    .read_file(&record_name, /*maximum_bytes*/ 256)?
                    != cleanup::reservation_record(name)
                {
                    bail!("workflow backup reservation conflicts with transaction");
                }
            }
            Err(Errno::NOENT) => store.backups.write_file(
                &record_name,
                &cleanup::reservation_record(name),
                /*replace*/ false,
            )?,
            Err(error) => return Err(error).context("failed to inspect workflow backup record"),
        }
        active_parent.rename_child_noreplace(active_name, &store.backups, name)?;
        store.backups.existing_child(name)?
    };
    verify_backup(&backup, journal)?;
    let metadata = rustix::fs::fstat(backup.handle())?;
    let bound = cleanup::bound_record(name, device_id_from_stat(metadata.st_dev), metadata.st_ino);
    let current_record = store
        .backups
        .read_file(&record_name, /*maximum_bytes*/ 256)?;
    if current_record == cleanup::reservation_record(name) {
        store
            .backups
            .write_file(&record_name, &bound, /*replace*/ true)?;
    } else if current_record != bound {
        bail!("workflow backup ownership record does not match directory");
    }
    match statat(
        backup.handle(),
        ".codex-managed-operation",
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => {
            if backup.read_file(".codex-managed-operation", /*maximum_bytes*/ 128)?
                != name.as_bytes()
            {
                bail!("workflow backup operation marker does not match transaction");
            }
        }
        Err(Errno::NOENT) => backup.write_file(
            ".codex-managed-operation",
            name.as_bytes(),
            /*replace*/ false,
        )?,
        Err(error) => return Err(error).context("failed to inspect workflow backup marker"),
    }
    Ok(())
}

pub(super) fn verify_old_active(
    directory: &SecureDirectory,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    verify_old_release(directory, journal, OldReleaseLocation::Active)
}

pub(super) fn verify_backup(
    directory: &SecureDirectory,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    verify_old_release(directory, journal, OldReleaseLocation::Backup)
}

enum OldReleaseLocation {
    Active,
    Backup,
}

fn verify_old_release(
    directory: &SecureDirectory,
    journal: &ManagedWorkflowJournal,
    location: OldReleaseLocation,
) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let retained = rustix::fs::fstat(directory.handle())?;
    let named = std::fs::symlink_metadata(directory.path().as_path())?;
    if !named.is_dir()
        || named.dev() != device_id_from_stat(retained.st_dev)
        || named.ino() != retained.st_ino
    {
        bail!("previous workflow release changed directory identity");
    }
    let previous = journal
        .previous_receipt
        .as_ref()
        .context("replacement journal has no previous receipt")?;
    let marker = read_marker(directory)?;
    if marker.id != journal.id || marker.release != previous.installed {
        bail!("workflow backup marker does not match previous receipt");
    }
    let deadline =
        crate::runner::CommandDeadline::after(std::time::Duration::from_secs(/*secs*/ 60));
    let evidence = match location {
        OldReleaseLocation::Active => published_payload_evidence(
            directory.path().as_path(),
            VERIFICATION_LIMITS,
            deadline,
            /*cancelled*/ None,
        )?,
        OldReleaseLocation::Backup => backup_payload_evidence(
            directory.path().as_path(),
            VERIFICATION_LIMITS,
            deadline,
            /*cancelled*/ None,
        )?,
    };
    if evidence.sha256 != marker.evidence_digest {
        bail!("previous managed workflow payload differs from its marker");
    }
    let named = std::fs::symlink_metadata(directory.path().as_path())?;
    if !named.is_dir()
        || named.dev() != device_id_from_stat(retained.st_dev)
        || named.ino() != retained.st_ino
    {
        bail!("previous workflow release changed directory identity during verification");
    }
    Ok(())
}

pub(super) fn restore_previous(
    store: &ManagedWorkflowStore,
    journal: &ManagedWorkflowJournal,
    active_parent: &SecureDirectory,
    active_name: &str,
) -> anyhow::Result<()> {
    let name = journal.transaction_id.as_str();
    let backup = store.backups.existing_child(name)?;
    verify_backup(&backup, journal)?;
    match statat(
        backup.handle(),
        ".codex-managed-operation",
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => {
            if backup.read_file(".codex-managed-operation", /*maximum_bytes*/ 128)?
                != name.as_bytes()
            {
                bail!("workflow backup operation marker does not match transaction");
            }
            backup.remove_regular_file(".codex-managed-operation")?;
        }
        Err(Errno::NOENT) => {}
        Err(error) => return Err(error).context("failed to inspect backup operation marker"),
    }
    drop(backup);
    store
        .backups
        .rename_child_noreplace(name, active_parent, active_name)?;
    cleanup_backup(store, journal)
}

pub(super) fn cleanup_backup(
    store: &ManagedWorkflowStore,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    let name = journal.transaction_id.as_str();
    let root = statat(store.backups.handle(), name, AtFlags::SYMLINK_NOFOLLOW);
    let (device, inode) = match root {
        Ok(metadata) => (device_id_from_stat(metadata.st_dev), metadata.st_ino),
        Err(Errno::NOENT) => (0, 0),
        Err(error) => return Err(error).context("failed to inspect workflow backup cleanup root"),
    };
    cleanup::remove_tree(
        &store.backups,
        name,
        device,
        inode,
        cleanup::OwnershipMarker::Required,
        cleanup::CleanupEntryLimit::STANDARD,
    )
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
