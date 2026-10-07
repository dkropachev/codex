mod catalog;
#[cfg(unix)]
mod cleanup;
mod copy;
mod fs;
mod journal;
mod lock;
mod prepare;
mod receipt;
mod stage;

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
    _global: ManagedFileLock,
    _workflow: ManagedFileLock,
}

pub(in crate::managed) struct ManagedWorkflowRunGuard {
    _global: ManagedFileLock,
    _workflow: ManagedFileLock,
}

pub(in crate::managed) struct ManagedWorkflowRecoveryGuard {
    _global: ManagedFileLock,
}

impl ManagedWorkflowStore {
    /// Copies a verified release into operation-private staging and binds it to a journal marker.
    #[expect(
        clippy::too_many_arguments,
        reason = "keep transaction inputs explicit across the staging boundary"
    )]
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
        #[cfg(unix)]
        if home.device_id()? != active_root.device_id()? {
            bail!("managed workflow roots must share a filesystem");
        }
        #[cfg(unix)]
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
        #[cfg(unix)]
        if management.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let locks = management.child("locks")?;
        #[cfg(unix)]
        if locks.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let receipts = management.child("receipts")?;
        #[cfg(unix)]
        if receipts.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let journals = management.child("journals")?;
        #[cfg(unix)]
        if journals.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let staging = management.child("staging")?;
        #[cfg(unix)]
        if staging.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        let backups = management.child("backups")?;
        #[cfg(unix)]
        if backups.device_id()? != active_root.device_id()? {
            bail!("managed workflow metadata crossed a filesystem boundary");
        }
        Ok(Self {
            management,
            locks,
            receipts,
            journals,
            staging,
            backups,
            active_root,
        })
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
        Ok(LockedManagedWorkflow {
            id: id.to_owned(),
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
