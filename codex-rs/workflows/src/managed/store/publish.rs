use anyhow::Context;
use anyhow::bail;
use rustix::fs::AtFlags;
use rustix::fs::statat;
use rustix::io::Errno;

use super::LockedManagedWorkflow;
use super::ManagedWorkflowStore;
use super::cleanup;
use super::expected::ExpectedCurrent;
use super::expected::compare_current;
use super::fs::SecureDirectory;
use super::fs::device_id_from_stat;
use super::journal::ManagedWorkflowJournal;
use super::journal::ManagedWorkflowNextAction;
use super::journal::journal_file_name;
use super::journal::read_journal;
use super::journal::read_marker;
use super::journal::write_journal;
use super::prepare::PreparedWorkflowRelease;
use super::receipt::read_pending_receipt;
use super::receipt::write_receipt;
use crate::managed::fetch::VERIFICATION_LIMITS;
use crate::managed::integrity::verify_published_copy;

/// A committed receipt may still require idempotent private cleanup.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::managed) enum ManagedWorkflowCommitOutcome {
    Committed,
    CommittedCleanupPending,
    RolledBack,
    RolledBackCleanupPending,
}

pub(super) fn commit_fresh(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
    expected: &ExpectedCurrent,
    mut prepared: PreparedWorkflowRelease<'_>,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    if prepared.journal.id != locked.id || prepared.journal.previous_receipt.is_some() {
        bail!("prepared workflow transaction does not match fresh install lock");
    }
    validate_prepared(store, &prepared)?;
    if journal_exists(&store.journals, &locked.id)? {
        store.recover_locked(locked)?;
        if journal_exists(&store.journals, &locked.id)? {
            bail!("managed workflow has an unresolved transaction journal");
        }
    }
    let previous = compare_current(&store.receipts, &locked.id, expected)?;
    if previous.is_some() || !matches!(expected, ExpectedCurrent::Absent) {
        bail!("fresh workflow install requires an absent receipt");
    }
    if let Some(parent) = active_parent(&store.active_root, &locked.id, ParentMode::Existing)?
        && parent
            .directory()
            .optional_existing_child(leaf(&locked.id)?)?
            .is_some()
    {
        bail!("managed workflow active target already exists");
    }
    if let Err(error) = write_journal(&store.journals, &prepared.journal, /*replace*/ false) {
        // The atomic rename may have succeeded before a directory sync failed.
        // Keep the payload whenever journal publication is ambiguous.
        if !matches!(journal_exists(&store.journals, &locked.id), Ok(false)) {
            prepared.staging.retain_for_recovery();
        }
        return Err(error);
    }
    prepared.staging.retain_for_recovery();
    let journal = prepared.journal;
    drop(prepared.staging);
    finish_fresh(store, journal)
}

pub(super) fn validate_prepared(
    store: &ManagedWorkflowStore,
    prepared: &PreparedWorkflowRelease<'_>,
) -> anyhow::Result<()> {
    let prepared_parent = rustix::fs::fstat(prepared.staging.parent.handle())?;
    let store_parent = rustix::fs::fstat(store.staging.handle())?;
    if (prepared_parent.st_dev, prepared_parent.st_ino)
        != (store_parent.st_dev, store_parent.st_ino)
        || prepared.journal.transaction_id != prepared.staging.name()
    {
        bail!("prepared workflow release belongs to a different transaction store");
    }
    Ok(())
}

pub(super) fn recover_fresh(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
    if !journal_exists(&store.journals, &locked.id)? {
        return Ok(None);
    }
    let journal = read_journal(&store.journals, &locked.id)?;
    if journal.previous_receipt.is_some() {
        bail!("replacement journal requires replacement recovery");
    }
    finish_fresh(store, journal).map(Some)
}

pub(super) fn finish_fresh(
    store: &ManagedWorkflowStore,
    mut journal: ManagedWorkflowJournal,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    let staging = store
        .staging
        .optional_existing_child(&journal.transaction_id)?;
    let active = active_parent(&store.active_root, &journal.id, ParentMode::Existing)?;
    let current = if let Some(parent) = &active {
        parent
            .directory()
            .optional_existing_child(leaf(&journal.id)?)?
    } else {
        None
    };
    let current_receipt = read_pending_receipt(&store.receipts, &journal.id)?;
    let pending_present = if let Some(stage) = &staging {
        stage.optional_existing_child("payload")?.is_some()
    } else {
        false
    };
    if let Some(receipt) = &current_receipt {
        if receipt != &journal.next_receipt {
            bail!("managed workflow receipt conflicts with transaction journal");
        }
        if current.is_none() {
            bail!("committed managed workflow receipt has no active release");
        }
    }
    match journal.next_action {
        ManagedWorkflowNextAction::PublishRelease if current_receipt.is_some() => {
            bail!("publish action conflicts with an already committed receipt")
        }
        ManagedWorkflowNextAction::WriteReceipt if current.is_none() => {
            bail!("receipt action has no published release")
        }
        ManagedWorkflowNextAction::Cleanup if current.is_none() || current_receipt.is_none() => {
            bail!("cleanup action has no committed active release")
        }
        ManagedWorkflowNextAction::MoveCurrentAside => {
            bail!("fresh install has a replacement action")
        }
        ManagedWorkflowNextAction::PublishRelease
        | ManagedWorkflowNextAction::WriteReceipt
        | ManagedWorkflowNextAction::Cleanup => {}
    }
    if let Some(target) = &current {
        verify_published(target, &journal)?;
        if pending_present {
            bail!("transaction has both prepared and published payloads");
        }
    } else {
        if !pending_present {
            bail!("managed workflow has no pending payload or active release");
        }
        let stage = staging
            .as_ref()
            .context("managed workflow has no pending payload or active release")?;
        let payload = stage.existing_child("payload")?;
        verify_published(&payload, &journal)?;
        drop(payload);
        let parent = active_parent(&store.active_root, &journal.id, ParentMode::Create)?
            .context("active workflow parent could not be created")?;
        stage.rename_child_noreplace("payload", parent.directory(), leaf(&journal.id)?)?;
        let published = parent.directory().existing_child(leaf(&journal.id)?)?;
        verify_published(&published, &journal)?;
    }
    if journal.next_action == ManagedWorkflowNextAction::PublishRelease {
        journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
        write_journal(&store.journals, &journal, /*replace*/ true)?;
    }
    match current_receipt {
        Some(receipt) if receipt == journal.next_receipt => {}
        Some(_) => bail!("managed workflow receipt conflicts with transaction journal"),
        None => {
            if let Err(error) = write_receipt(
                &store.receipts,
                &journal.next_receipt,
                /*replace*/ false,
            ) {
                if matches!(
                    read_pending_receipt(&store.receipts, &journal.id),
                    Ok(Some(receipt)) if receipt == journal.next_receipt
                ) {
                    let parent =
                        active_parent(&store.active_root, &journal.id, ParentMode::Existing)?
                            .context("committed workflow active parent disappeared")?;
                    let target = parent.directory().existing_child(leaf(&journal.id)?)?;
                    verify_published(&target, &journal)?;
                    return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
                }
                return Err(error);
            }
        }
    }
    if journal.next_action != ManagedWorkflowNextAction::Cleanup {
        journal.next_action = ManagedWorkflowNextAction::Cleanup;
        if write_journal(&store.journals, &journal, /*replace*/ true).is_err() {
            return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
        }
    }
    drop(staging);
    match cleanup_fresh(store, &journal) {
        Ok(()) => Ok(ManagedWorkflowCommitOutcome::Committed),
        Err(_) => Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending),
    }
}

pub(super) fn verify_published(
    directory: &SecureDirectory,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let retained = rustix::fs::fstat(directory.handle())?;
    let named = std::fs::symlink_metadata(directory.path().as_path())?;
    if !named.is_dir()
        || named.dev() != device_id_from_stat(retained.st_dev)
        || named.ino() != retained.st_ino
    {
        bail!("managed workflow publication changed directory identity");
    }
    if !read_marker(directory)?.matches_journal(journal) {
        bail!("managed workflow marker does not match transaction journal");
    }
    verify_published_copy(
        directory.path().as_path(),
        &journal.evidence,
        VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(std::time::Duration::from_secs(/*secs*/ 60)),
        /*cancelled*/ None,
    )?;
    let named = std::fs::symlink_metadata(directory.path().as_path())?;
    if !named.is_dir()
        || named.dev() != device_id_from_stat(retained.st_dev)
        || named.ino() != retained.st_ino
    {
        bail!("managed workflow publication changed directory identity during verification");
    }
    Ok(())
}

fn cleanup_fresh(
    store: &ManagedWorkflowStore,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    cleanup_stage(store, journal)?;
    store
        .journals
        .remove_regular_file(&journal_file_name(&journal.id)?)
}

pub(super) fn cleanup_stage(
    store: &ManagedWorkflowStore,
    journal: &ManagedWorkflowJournal,
) -> anyhow::Result<()> {
    let root = statat(
        store.staging.handle(),
        journal.transaction_id.as_str(),
        AtFlags::SYMLINK_NOFOLLOW,
    );
    let (device, inode) = match root {
        Ok(metadata) => (device_id_from_stat(metadata.st_dev), metadata.st_ino),
        Err(Errno::NOENT) => (0, 0),
        Err(error) => return Err(error).context("failed to inspect transaction cleanup root"),
    };
    cleanup::remove_tree(
        &store.staging,
        &journal.transaction_id,
        device,
        inode,
        cleanup::OwnershipMarker::Required,
        cleanup::CleanupEntryLimit::STANDARD,
    )
}

pub(super) fn journal_exists(journals: &SecureDirectory, id: &str) -> anyhow::Result<bool> {
    match statat(
        journals.handle(),
        journal_file_name(id)?.as_str(),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(error) => Err(error).context("failed to inspect managed workflow journal"),
    }
}

pub(super) enum ParentMode {
    Existing,
    Create,
}

pub(super) enum ActiveParent<'a> {
    Root(&'a SecureDirectory),
    Child(SecureDirectory),
}

impl ActiveParent<'_> {
    pub(super) fn directory(&self) -> &SecureDirectory {
        match self {
            Self::Root(root) => root,
            Self::Child(child) => child,
        }
    }
}

pub(super) fn active_parent<'a>(
    root: &'a SecureDirectory,
    id: &str,
    mode: ParentMode,
) -> anyhow::Result<Option<ActiveParent<'a>>> {
    let mut parent = ActiveParent::Root(root);
    for component in id.split('/').take(id.split('/').count().saturating_sub(1)) {
        let current = parent.directory();
        parent = match mode {
            ParentMode::Existing => match current.optional_existing_child(component)? {
                Some(child) => ActiveParent::Child(child),
                None => return Ok(None),
            },
            ParentMode::Create => ActiveParent::Child(current.child(component)?),
        };
    }
    Ok(Some(parent))
}

pub(super) fn leaf(id: &str) -> anyhow::Result<&str> {
    id.rsplit('/')
        .next()
        .context("managed workflow id is empty")
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod tests;
