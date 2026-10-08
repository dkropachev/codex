use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::bail;

use super::LockedManagedWorkflow;
use super::ManagedWorkflowStore;
use super::journal::ManagedWorkflowOperation;
use super::journal::read_journal;
use super::journal::read_journal_named;
use super::operation::recover_marked_bun_operations;
use super::publish;
use super::publish::ManagedWorkflowCommitOutcome;
use super::replace;
use super::uninstall;

const MAX_JOURNAL_DIRECTORY_ENTRIES: usize = 4_096;
const MAX_JOURNALS: usize = 1_024;

/// Recovers all durable transactions while the global lock excludes operations.
pub(super) fn recover_all(
    store: &ManagedWorkflowStore,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let _global = store.lock_recovery(cancelled)?;
    let mut names = store
        .journals
        .list_names(MAX_JOURNAL_DIRECTORY_ENTRIES, cancelled)?;
    if names.len() > MAX_JOURNALS {
        bail!("managed workflow journal count exceeds its limit");
    }
    names.sort();
    let mut journals = Vec::with_capacity(names.len());
    for name in names {
        if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
            bail!("managed workflow recovery was cancelled");
        }
        journals.push(read_journal_named(&store.journals, &name)?);
    }
    for journal in journals {
        if cancelled.is_some_and(|signal| signal.load(Ordering::Relaxed)) {
            bail!("managed workflow recovery was cancelled");
        }
        match journal.operation {
            ManagedWorkflowOperation::Install => publish::finish_fresh(store, journal)?,
            ManagedWorkflowOperation::Replace => replace::finish_replace(store, journal)?,
            ManagedWorkflowOperation::Uninstall => uninstall::finish_uninstall(store, journal)?,
        };
    }
    recover_marked_bun_operations(&store.management, cancelled)?;
    Ok(())
}

/// Resumes a journal for one ID while its exclusive workflow lock is retained.
pub(super) fn recover_locked(
    store: &ManagedWorkflowStore,
    locked: &LockedManagedWorkflow,
) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
    locked.ensure_store(store)?;
    if !publish::journal_exists(&store.journals, &locked.id)? {
        return Ok(None);
    }
    let journal = read_journal(&store.journals, &locked.id)?;
    match journal.operation {
        ManagedWorkflowOperation::Install => publish::finish_fresh(store, journal).map(Some),
        ManagedWorkflowOperation::Replace => replace::finish_replace(store, journal).map(Some),
        ManagedWorkflowOperation::Uninstall => {
            uninstall::finish_uninstall(store, journal).map(Some)
        }
    }
}

#[cfg(all(test, unix))]
#[path = "recovery_tests.rs"]
mod tests;
