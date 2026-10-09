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
use super::journal::read_marker;
use super::journal::write_journal;
use super::publish;
use super::publish::ManagedWorkflowCommitOutcome;
use super::publish::ParentMode;
use super::publish::active_parent;
use super::publish::leaf;
use super::receipt::read_pending_receipt;
use super::receipt::remove_receipt_and_empty_directories;
use super::stage::TransactionStaging;
use crate::managed::fetch::VERIFICATION_LIMITS;
use crate::managed::integrity::published_payload_evidence;

pub(super) fn commit_uninstall(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
    expected: &ExpectedCurrent,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    if publish::journal_exists(&store.journals, &locked.id)? {
        store.recover_locked(locked)?;
        if publish::journal_exists(&store.journals, &locked.id)? {
            bail!("managed workflow has an unresolved transaction journal");
        }
    }
    let receipt = compare_current(&store.receipts, &locked.id, expected)?
        .context("uninstall requires a managed workflow receipt")?;
    let parent = active_parent(&store.active_root, &locked.id, ParentMode::Existing)?
        .context("managed workflow active parent disappeared")?;
    let active = parent.directory().existing_child(leaf(&locked.id)?)?;
    let marker = read_marker(&active)?;
    if marker.id != locked.id || marker.release != receipt.installed {
        bail!("managed workflow active marker does not match its receipt");
    }
    let evidence = published_payload_evidence(
        active.path().as_path(),
        VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(std::time::Duration::from_secs(/*secs*/ 60)),
        /*cancelled*/ None,
    )?;
    if evidence.sha256 != marker.evidence_digest {
        bail!("managed workflow active payload differs from its marker");
    }
    drop(active);
    let mut staging = TransactionStaging::create(&store.staging)?;
    let journal =
        ManagedWorkflowJournal::new_uninstall(staging.name().to_owned(), receipt, evidence)?;
    let active = parent.directory().existing_child(leaf(&locked.id)?)?;
    backup::verify_old_active(&active, &journal)?;
    drop(active);
    if let Err(error) = write_journal(&store.journals, &journal, /*replace*/ false) {
        if !matches!(
            publish::journal_exists(&store.journals, &locked.id),
            Ok(false)
        ) {
            staging.retain_for_recovery();
        }
        return Err(error);
    }
    staging.retain_for_recovery();
    drop(staging);
    finish_uninstall(store, journal)
}

pub(super) fn finish_uninstall(
    store: &ManagedWorkflowStore,
    mut journal: ManagedWorkflowJournal,
) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
    if journal.operation != ManagedWorkflowOperation::Uninstall {
        bail!("uninstall recovery requires an uninstall journal");
    }
    let previous = journal
        .previous_receipt
        .as_ref()
        .context("uninstall journal has no previous receipt")?;
    let parent = active_parent(&store.active_root, &journal.id, ParentMode::Existing)?
        .context("managed workflow active parent disappeared")?;
    let active = parent
        .directory()
        .optional_existing_child(leaf(&journal.id)?)?;
    let backup = store
        .backups
        .optional_existing_child(&journal.transaction_id)?;
    let receipt = read_pending_receipt(&store.receipts, &journal.id)?;
    if receipt.as_ref().is_some_and(|current| current != previous) {
        bail!("uninstall receipt conflicts with transaction journal");
    }
    if active.is_some() && backup.is_some() {
        bail!("uninstall has both active release and backup");
    }
    if active.is_some() && receipt.is_none() {
        bail!("uninstall removed its receipt while the release is active");
    }
    if backup.is_none()
        && active.is_none()
        && (receipt.is_some() || journal.next_action != ManagedWorkflowNextAction::Cleanup)
    {
        bail!("uninstall has neither active release nor backup");
    }
    if let Some(active) = &active {
        backup::verify_old_active(active, &journal)?;
        if read_marker(active)?.evidence_digest != journal.evidence.sha256 {
            bail!("uninstall active payload conflicts with transaction evidence");
        }
    }
    if let Some(backup) = &backup
        && journal.next_action != ManagedWorkflowNextAction::Cleanup
    {
        backup::verify_backup(backup, &journal)?;
        if read_marker(backup)?.evidence_digest != journal.evidence.sha256 {
            bail!("uninstall backup conflicts with transaction evidence");
        }
    }
    match journal.next_action {
        ManagedWorkflowNextAction::MoveCurrentAside if receipt.is_none() => {
            bail!("uninstall move action has no receipt")
        }
        ManagedWorkflowNextAction::RemoveReceipt if active.is_some() || backup.is_none() => {
            bail!("uninstall receipt action has no backup")
        }
        ManagedWorkflowNextAction::Cleanup if active.is_some() || receipt.is_some() => {
            bail!("uninstall cleanup has an active release or receipt")
        }
        ManagedWorkflowNextAction::PublishRelease | ManagedWorkflowNextAction::WriteReceipt => {
            bail!("uninstall journal has an invalid action")
        }
        ManagedWorkflowNextAction::MoveCurrentAside
        | ManagedWorkflowNextAction::RemoveReceipt
        | ManagedWorkflowNextAction::Cleanup => {}
    }
    drop(active);
    drop(backup);
    if journal.next_action == ManagedWorkflowNextAction::MoveCurrentAside {
        backup::move_current_aside(store, &journal, parent.directory(), leaf(&journal.id)?)?;
        journal.next_action = ManagedWorkflowNextAction::RemoveReceipt;
        write_journal(&store.journals, &journal, /*replace*/ true)?;
    }
    if journal.next_action == ManagedWorkflowNextAction::RemoveReceipt {
        remove_receipt_and_empty_directories(&store.receipts, &journal.id)?;
        journal.next_action = ManagedWorkflowNextAction::Cleanup;
        if write_journal(&store.journals, &journal, /*replace*/ true).is_err() {
            return Ok(ManagedWorkflowCommitOutcome::CommittedCleanupPending);
        }
    }
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
#[path = "uninstall_tests.rs"]
mod tests;
