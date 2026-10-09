use std::sync::atomic::AtomicBool;

use anyhow::bail;

use super::ManagedWorkflowService;
use super::WorkflowReleaseIdentity;
use crate::managed::store::ExpectedCurrent;
use crate::managed::store::ManagedWorkflowCommitOutcome;
use crate::managed::store::ReceiptIdentity;

/// The removed workflow and whether durable transaction cleanup remains pending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedWorkflowUninstallation {
    pub id: String,
    pub cleanup_pending: bool,
}

impl ManagedWorkflowService {
    /// Removes a verified managed release only when its installed identity is still current.
    pub fn uninstall(
        &self,
        id: &str,
        expected_installed: &WorkflowReleaseIdentity,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<ManagedWorkflowUninstallation> {
        let locked = self.store.lock_install(id, Some(cancelled))?;
        let previous = self
            .store
            .read_locked_verified_receipt(&locked, cancelled)?;
        if WorkflowReleaseIdentity::from(&previous.installed) != *expected_installed {
            bail!("managed workflow release changed before uninstall");
        }
        let expected = ExpectedCurrent::Receipt(ReceiptIdentity::from_receipt(&previous)?);
        let outcome = self.store.commit_uninstall(&locked, &expected)?;
        let cleanup_pending = match outcome {
            ManagedWorkflowCommitOutcome::Committed => false,
            ManagedWorkflowCommitOutcome::CommittedCleanupPending => true,
            ManagedWorkflowCommitOutcome::RolledBack
            | ManagedWorkflowCommitOutcome::RolledBackCleanupPending => {
                bail!("managed workflow uninstall rolled back");
            }
        };
        Ok(ManagedWorkflowUninstallation {
            id: id.to_owned(),
            cleanup_pending,
        })
    }
}

#[cfg(test)]
#[path = "uninstall_tests.rs"]
mod tests;
