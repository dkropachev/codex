use anyhow::Context;
use anyhow::bail;

use super::ManagedWorkflowStore;
use super::cleanup;
use super::fs::SecureDirectory;
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
        if store.backups.child_exists(&record_name)? {
            if store.backups.read_file(&record_name, 256)? != cleanup::reservation_record(name) {
                bail!("workflow backup reservation conflicts with transaction");
            }
        } else {
            store.backups.write_file(
                &record_name,
                &cleanup::reservation_record(name),
                /*replace*/ false,
            )?;
        }
        active_parent.rename_child_noreplace(active_name, &store.backups, name)?;
        store.backups.existing_child(name)?
    };
    verify_backup(&backup, journal)?;
    let (device, inode) = backup.identity()?;
    let bound = cleanup::bound_record(name, device, inode);
    let current_record = store.backups.read_file(&record_name, 256)?;
    if current_record == cleanup::reservation_record(name) {
        store
            .backups
            .write_file(&record_name, &bound, /*replace*/ true)?;
    } else if current_record != bound {
        bail!("workflow backup ownership record does not match directory");
    }
    if backup.child_exists(".codex-managed-operation")? {
        if backup.read_file(".codex-managed-operation", 128)? != name.as_bytes() {
            bail!("workflow backup operation marker does not match transaction");
        }
    } else {
        backup.write_file(
            ".codex-managed-operation",
            name.as_bytes(),
            /*replace*/ false,
        )?;
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
    let retained = directory.identity()?;
    let named = SecureDirectory::open_root(directory.path())?;
    if named.identity()? != retained {
        bail!("previous workflow release changed directory identity");
    }
    drop(named);
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
    let named = SecureDirectory::open_root(directory.path())?;
    if named.identity()? != retained {
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
    if backup.child_exists(".codex-managed-operation")? {
        if backup.read_file(".codex-managed-operation", 128)? != name.as_bytes() {
            bail!("workflow backup operation marker does not match transaction");
        }
        backup.remove_regular_file(".codex-managed-operation")?;
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
    let (device, inode) = match store.backups.optional_existing_child(name)? {
        Some(root) => root.identity()?,
        None => (0, 0),
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

#[cfg(all(test, unix))]
#[path = "backup_tests.rs"]
mod tests;
