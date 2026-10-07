#[cfg(unix)]
mod backup;
mod catalog;
#[cfg(unix)]
mod cleanup;
#[cfg(windows)]
#[path = "store/cleanup/windows.rs"]
mod cleanup;
mod copy;
#[cfg(any(unix, windows))]
mod expected;
mod fs;
mod journal;
mod lock;
mod operation;
mod prepare;
#[cfg(unix)]
mod publish;
mod receipt;
#[cfg(unix)]
mod recovery;
#[cfg(unix)]
mod replace;
mod stage;
#[cfg(windows)]
mod windows_security;

use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::managed::fetch::VerificationLimits;
use crate::managed::integrity::VerifiedWorkflowRelease;
use fs::SecureDirectory;
use lock::LockMode;
use lock::ManagedFileLock;
use prepare::PreparedWorkflowRelease;
use receipt::ManagedWorkflowReceipt;

pub(in crate::managed) use operation::ManagedBunOperationDirectory;

#[cfg(any(unix, windows))]
pub(in crate::managed) use expected::ExpectedCurrent;
#[cfg(unix)]
pub(in crate::managed) use publish::ManagedWorkflowCommitOutcome;

/// Owns the private, same-filesystem metadata layout for managed workflows.
pub(in crate::managed) struct ManagedWorkflowStore {
    management: SecureDirectory,
    locks: SecureDirectory,
    receipts: SecureDirectory,
    journals: SecureDirectory,
    staging: SecureDirectory,
    backups: SecureDirectory,
    active_root: SecureDirectory,
}

pub(in crate::managed) struct LockedManagedWorkflow {
    pub(in crate::managed) id: String,
    #[cfg(unix)]
    management_identity: (u64, u64),
    _global: ManagedFileLock,
    _workflow: ManagedFileLock,
}

#[cfg(unix)]
impl LockedManagedWorkflow {
    fn ensure_store(&self, store: &ManagedWorkflowStore) -> anyhow::Result<()> {
        let metadata = rustix::fs::fstat(store.management.handle())?;
        if self.management_identity != (metadata.st_dev, metadata.st_ino) {
            bail!("managed workflow lock belongs to a different store");
        }
        Ok(())
    }
}

pub(in crate::managed) struct ManagedWorkflowRunGuard {
    _global: ManagedFileLock,
    _workflow: ManagedFileLock,
}

pub(in crate::managed) struct ManagedWorkflowRecoveryGuard {
    _global: ManagedFileLock,
}

impl ManagedWorkflowStore {
    #[cfg(unix)]
    pub(in crate::managed) fn recover_locked(
        &self,
        locked: &LockedManagedWorkflow,
    ) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
        recovery::recover_locked(self, locked)
    }

    #[cfg(unix)]
    pub(in crate::managed) fn commit_fresh(
        &self,
        locked: &LockedManagedWorkflow,
        expected: &ExpectedCurrent,
        prepared: PreparedWorkflowRelease<'_>,
    ) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
        locked.ensure_store(self)?;
        publish::commit_fresh(self, locked, expected, prepared)
    }

    #[cfg(unix)]
    pub(in crate::managed) fn recover_fresh(
        &self,
        locked: &LockedManagedWorkflow,
    ) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
        locked.ensure_store(self)?;
        publish::recover_fresh(self, locked)
    }

    #[cfg(unix)]
    pub(in crate::managed) fn commit_replace(
        &self,
        locked: &LockedManagedWorkflow,
        expected: &ExpectedCurrent,
        prepared: PreparedWorkflowRelease<'_>,
    ) -> anyhow::Result<ManagedWorkflowCommitOutcome> {
        locked.ensure_store(self)?;
        replace::commit_replace(self, locked, expected, prepared)
    }

    #[cfg(unix)]
    pub(in crate::managed) fn recover_replace(
        &self,
        locked: &LockedManagedWorkflow,
    ) -> anyhow::Result<Option<ManagedWorkflowCommitOutcome>> {
        locked.ensure_store(self)?;
        replace::recover_replace(self, locked)
    }

    /// Copies a verified release into operation-private staging and binds it to a journal marker.
    pub(in crate::managed) fn prepare_release(
        &self,
        locked: &LockedManagedWorkflow,
        verified: VerifiedWorkflowRelease,
        previous_receipt: Option<ManagedWorkflowReceipt>,
        next_receipt: ManagedWorkflowReceipt,
        limits: VerificationLimits,
        deadline: crate::runner::CommandDeadline,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<PreparedWorkflowRelease<'_>> {
        #[cfg(unix)]
        locked.ensure_store(self)?;
        prepare::prepare_release(
            &self.staging,
            locked,
            verified,
            previous_receipt,
            next_receipt,
            limits,
            deadline,
            cancelled,
        )
    }
    /// Reads a consistent receipt catalog after active workflow mutations finish.
    pub(in crate::managed) fn list_receipts(
        &self,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<Vec<ManagedWorkflowReceipt>> {
        let global = self.management.open_lock_file("managed.lock")?;
        let _global = ManagedFileLock::acquire(global, LockMode::Exclusive, cancelled)?;
        catalog::collect_receipts(&self.receipts, cancelled)
    }
    pub(in crate::managed) fn create(
        codex_home: &AbsolutePathBuf,
        workflow_root: &AbsolutePathBuf,
    ) -> anyhow::Result<Self> {
        let home = SecureDirectory::open_root(codex_home)?;
        let active_root = SecureDirectory::open_root(workflow_root)?;
        #[cfg(any(unix, windows))]
        if home.device_id()? != active_root.device_id()? {
            bail!("managed workflow roots must share a filesystem");
        }
        #[cfg(any(unix, windows))]
        if let Some(existing) = home.optional_existing_child(".workflow-management")? {
            if existing.device_id()? != active_root.device_id()? {
                bail!("managed workflow metadata crossed a filesystem boundary");
            }
            for name in ["locks", "receipts", "journals", "staging", "backups"] {
                if let Some(child) = existing.optional_existing_child(name)?
                    && child.device_id()? != active_root.device_id()?
                {
                    bail!("managed workflow metadata crossed a filesystem boundary");
                }
            }
        }
        let management = home.child(".workflow-management")?;
        #[cfg(any(unix, windows))]
        if management.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let locks = management.child("locks")?;
        #[cfg(any(unix, windows))]
        if locks.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let receipts = management.child("receipts")?;
        #[cfg(any(unix, windows))]
        if receipts.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let journals = management.child("journals")?;
        #[cfg(any(unix, windows))]
        if journals.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let staging = management.child("staging")?;
        #[cfg(any(unix, windows))]
        if staging.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let backups = management.child("backups")?;
        #[cfg(any(unix, windows))]
        if backups.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let store = Self {
            management,
            locks,
            receipts,
            journals,
            staging,
            backups,
            active_root,
        };
        #[cfg(unix)]
        recovery::recover_all(&store, /*cancelled*/ None)?;
        Ok(store)
    }

    pub(in crate::managed) fn lock_install(
        &self,
        id: &str,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<LockedManagedWorkflow> {
        receipt::validate_id(id)?;
        let global = self.management.open_lock_file("managed.lock")?;
        let global = ManagedFileLock::acquire(global, LockMode::Shared, cancelled)?;
        let workflow = self.workflow_lock_file(id)?;
        let workflow = ManagedFileLock::acquire(workflow, LockMode::Exclusive, cancelled)?;
        #[cfg(unix)]
        let management_identity = {
            let metadata = rustix::fs::fstat(self.management.handle())?;
            (metadata.st_dev, metadata.st_ino)
        };
        Ok(LockedManagedWorkflow {
            id: id.to_owned(),
            #[cfg(unix)]
            management_identity,
            _global: global,
            _workflow: workflow,
        })
    }

    pub(in crate::managed) fn lock_run(
        &self,
        id: &str,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<ManagedWorkflowRunGuard> {
        receipt::validate_id(id)?;
        let global = self.management.open_lock_file("managed.lock")?;
        let global = ManagedFileLock::acquire(global, LockMode::Shared, cancelled)?;
        let workflow = self.workflow_lock_file(id)?;
        let workflow = ManagedFileLock::acquire(workflow, LockMode::Shared, cancelled)?;
        Ok(ManagedWorkflowRunGuard {
            _global: global,
            _workflow: workflow,
        })
    }

    pub(in crate::managed) fn lock_recovery(
        &self,
        cancelled: Option<&AtomicBool>,
    ) -> anyhow::Result<ManagedWorkflowRecoveryGuard> {
        let global = self.management.open_lock_file("managed.lock")?;
        let global = ManagedFileLock::acquire(global, LockMode::Exclusive, cancelled)?;
        Ok(ManagedWorkflowRecoveryGuard { _global: global })
    }

    fn workflow_lock_file(&self, id: &str) -> anyhow::Result<std::fs::File> {
        let mut components = id.split('/').collect::<Vec<_>>();
        let leaf = components
            .pop()
            .context("managed workflow id has no component")?;
        let mut directory = None;
        for component in components {
            directory = Some(directory.as_ref().unwrap_or(&self.locks).child(component)?);
        }
        directory
            .as_ref()
            .unwrap_or(&self.locks)
            .open_lock_file(&format!("{leaf}.lock"))
    }
}

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;
