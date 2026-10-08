use anyhow::Context;
use anyhow::bail;

use super::LockedManagedWorkflow;
use super::ManagedWorkflowStore;
use super::backup;
use super::expected::ExpectedCurrent;
use super::expected::compare_current;
use super::journal::ManagedWorkflowJournal;
use super::journal::ManagedWorkflowNextAction;
use super::journal::ManagedWorkflowOperation;
use super::journal::journal_file_name;
use super::journal::read_journal;
use super::journal::read_marker;
use super::journal::write_journal;
use super::prepare::PreparedWorkflowRelease;
use super::publish;
use super::publish::ManagedWorkflowCommitOutcome;
use super::publish::ParentMode;
use super::publish::active_parent;
use super::publish::leaf;
use super::publish::verify_published;
use super::receipt::read_pending_receipt;
use super::receipt::write_receipt;

pub(super) fn commit_replace(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
    expected: &ExpectedCurrent,
    mut prepared: PreparedWorkflowRelease<'_>,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    if prepared.journal.id != locked.id || prepared.journal.previous_receipt.is_none() {
        bail!("prepared workflow transaction does not match replacement lock");
    }
    publish::validate_prepared(store, &prepared)?;
    if publish::journal_exists(&store.journals, &locked.id)? {
        store.recover_locked(locked)?;
        if publish::journal_exists(&store.journals, &locked.id)? {
            bail!("managed workflow has an unresolved transaction journal");
        }
    }
    let current = compare_current(&store.receipts, &locked.id, expected)?;
    if current.as_ref() != prepared.journal.previous_receipt.as_ref() {
        bail!("replacement receipt does not match prepared previous release");
    }
    let parent = active_parent(&store.active_root, &locked.id, ParentMode::Existing)?
        .context("replacement active parent is missing")?;
    let active = parent.directory().existing_child(leaf(&locked.id)?)?;
    backup::verify_old_active(&active, &prepared.journal)?;
    drop(active);
    if let Err(error) = write_journal(&store.journals, &prepared.journal, /*replace*/ false) {
        if !matches!(
            publish::journal_exists(&store.journals, &locked.id),
            Ok(false)
        ) {
            prepared.staging.retain_for_recovery();
        }
        return Err(error);
    }
    prepared.staging.retain_for_recovery();
    let journal = prepared.journal;
    drop(prepared.staging);
    finish_replace(store, journal)
}

pub(super) fn recover_replace(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
    if !publish::journal_exists(&store.journals, &locked.id)? {
        return Ok(None);
    }
    let journal = read_journal(&store.journals, &locked.id)?;
    if journal.operation != ManagedWorkflowOperation::Replace {
        bail!("fresh-install journal requires fresh-install recovery");
    }
    finish_replace(store, journal).map(Some)
}

pub(super) fn finish_replace(
    store: &ManagedWorkflowStore,
    mut journal: ManagedWorkflowJournal,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    if journal.operation != ManagedWorkflowOperation::Replace {
        bail!("replacement recovery requires a replacement journal");
    }
    let previous = journal
        .previous_receipt
        .as_ref()
        .context("missing previous receipt")?;
    let parent = active_parent(&store.active_root, &journal.id, ParentMode::Existing)?
        .context("replacement active parent disappeared")?;
    let active = parent
        .directory()
        .optional_existing_child(leaf(&journal.id)?)?;
    let backup = store
        .backups
        .optional_existing_child(&journal.transaction_id)?;
    let staging = store
        .staging
        .optional_existing_child(&journal.transaction_id)?;
    let pending = if let Some(stage) = &staging {
        stage.optional_existing_child("payload")?.is_some()
    } else {
        false
    };
    let receipt = read_pending_receipt(&store.receipts, &journal.id)?
        .context("replacement receipt disappeared during transaction")?;
    if receipt != *previous && receipt != journal.next_receipt {
        bail!("replacement receipt conflicts with transaction journal");
    }
    let active_is_new = if let Some(active) = &active {
        let marker = read_marker(active)?;
        if marker.matches_journal(&journal) {
            verify_published(active, &journal)?;
            true
        } else if marker.id == journal.id && marker.release == previous.installed {
            false
        } else {
            bail!("replacement active marker conflicts with journal");
        }
    } else {
        false
    };
    if receipt == journal.next_receipt && !active_is_new {
        bail!("committed replacement receipt has no matching active release");
    }
    if !pending && !active_is_new && receipt == *previous {
        if !matches!(
            journal.next_action,
            ManagedWorkflowNextAction::MoveCurrentAside | ManagedWorkflowNextAction::PublishRelease
        ) {
            bail!("replacement pending payload vanished after publication");
        }
        if active.is_none() && backup.is_some() {
            drop(backup);
            backup::restore_previous(store, &journal, parent.directory(), leaf(&journal.id)?)?;
        } else if let Some(old) = &active
            && backup.is_none()
        {
            backup::verify_old_active(old, &journal)?;
        } else {
            bail!("replacement rollback topology is ambiguous");
        }
        drop(staging);
        if backup::cleanup_backup(store, &journal).is_err()
            || publish::cleanup_stage(store, &journal).is_err()
            || store
                .journals
                .remove_regular_file(&journal_file_name(&journal.id)?)
                .is_err()
        {
            return Ok(ManagedWorkflowCommitOutcome::RolledBackCleanupPending);
        }
        return Ok(ManagedWorkflowCommitOutcome::RolledBack);
    }
    match journal.next_action {
        ManagedWorkflowNextAction::MoveCurrentAside
            if backup.is_some() && active.is_some() || receipt == journal.next_receipt =>
        {
            bail!("move-aside action has conflicting transaction topology")
        }
        ManagedWorkflowNextAction::PublishRelease
            if backup.is_none() || receipt == journal.next_receipt =>
        {
            bail!("publish action has no backup or has committed receipt")
        }
        ManagedWorkflowNextAction::WriteReceipt if !active_is_new || backup.is_none() => {
            bail!("receipt action has no published replacement and backup")
        }
        ManagedWorkflowNextAction::Cleanup if !active_is_new || receipt != journal.next_receipt => {
            bail!("cleanup action has no committed replacement")
        }
        ManagedWorkflowNextAction::RemoveReceipt => {
            bail!("replacement journal has an uninstall action")
        }
        ManagedWorkflowNextAction::MoveCurrentAside
        | ManagedWorkflowNextAction::PublishRelease
        | ManagedWorkflowNextAction::WriteReceipt
        | ManagedWorkflowNextAction::Cleanup => {}
    }
    if active.is_some() && backup.is_some() && !active_is_new {
        bail!("replacement has both old active release and backup");
    }
    if active_is_new && pending {
        bail!("replacement has both pending and published payloads");
    }
    if journal.next_action == ManagedWorkflowNextAction::MoveCurrentAside {
        if active.is_none() && backup.is_none() {
            bail!("replacement has neither old release nor backup");
        }
        drop(active);
        backup::move_current_aside(store, &journal, parent.directory(), leaf(&journal.id)?)?;
        journal.next_action = ManagedWorkflowNextAction::PublishRelease;
        write_journal(&store.journals, &journal, /*replace*/ true)?;
    }
    if journal.next_action == ManagedWorkflowNextAction::PublishRelease {
        if !active_is_new {
            if !pending {
                bail!("replacement pending payload disappeared before publication");
            }
            let stage = staging
                .as_ref()
                .context("replacement staging disappeared")?;
            let payload = stage.existing_child("payload")?;
            verify_published(&payload, &journal)?;
            drop(payload);
            stage.rename_child_noreplace("payload", parent.directory(), leaf(&journal.id)?)?;
            let published = parent.directory().existing_child(leaf(&journal.id)?)?;
            verify_published(&published, &journal)?;
        }
        journal.next_action = ManagedWorkflowNextAction::WriteReceipt;
        write_journal(&store.journals, &journal, /*replace*/ true)?;
    }
    if receipt != journal.next_receipt
        && let Err(error) = write_receipt(
            &store.receipts,
            &journal.next_receipt,
            /*replace*/ true,
        )
    {
        if matches!(read_pending_receipt(&store.receipts, &journal.id), Ok(Some(next)) if next == journal.next_receipt)
        {
            return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
        }
        return Err(error);
    }
    if journal.next_action != ManagedWorkflowNextAction::Cleanup {
        journal.next_action = ManagedWorkflowNextAction::Cleanup;
        if write_journal(&store.journals, &journal, /*replace*/ true).is_err() {
            return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
        }
    }
    drop(backup);
    drop(staging);
    if backup::cleanup_backup(store, &journal).is_err()
        || publish::cleanup_stage(store, &journal).is_err()
        || store
            .journals
            .remove_regular_file(&journal_file_name(&journal.id)?)
            .is_err()
    {
        return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
    }
    Ok(ManagedWorkflowCommitOutcome::Committed)
}

#[cfg(all(test, unix))]
#[path = "replace_tests.rs"]
mod tests;
