use std::sync::atomic::AtomicBool;

use anyhow::bail;

use super::ManagedWorkflowService;
use super::update::ManagedWorkflowRecord;
use super::update::ManagedWorkflowUpdate;
use super::update::WorkflowReleaseIdentity;
use super::update::WorkflowUpdatePolicy;
use crate::managed::store::WorkflowRelease;

impl ManagedWorkflowService {
    /// Changes the update policy only if the caller's installed release is still current.
    pub fn set_policy(
        &self,
        id: &str,
        expected_installed: &WorkflowReleaseIdentity,
        policy: WorkflowUpdatePolicy,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<ManagedWorkflowRecord> {
        let locked = self.store.lock_install(id, Some(cancelled))?;
        let current = self.store.read_locked_receipt(&locked)?;
        if WorkflowReleaseIdentity::from(&current.installed) != *expected_installed {
            bail!("managed workflow release changed before policy mutation");
        }
        let mut next = current.clone();
        next.policy = policy;
        self.store.write_locked_receipt(&locked, &current, &next)?;
        Ok(ManagedWorkflowRecord::from(next))
    }

    /// Dismisses only the exact release the caller checked and still sees advertised.
    pub fn dismiss_release(
        &self,
        id: &str,
        expected_installed: &WorkflowReleaseIdentity,
        dismissed: &WorkflowReleaseIdentity,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<ManagedWorkflowRecord> {
        let locked = self.store.lock_install(id, Some(cancelled))?;
        let current = self.store.read_locked_receipt(&locked)?;
        if WorkflowReleaseIdentity::from(&current.installed) != *expected_installed {
            bail!("managed workflow release changed before dismissal");
        }
        let ManagedWorkflowUpdate::Available { release, .. } =
            super::update::check_release(&current, cancelled)?
        else {
            bail!("managed workflow has no eligible release to dismiss");
        };
        if &release != dismissed {
            bail!("managed workflow release changed before dismissal");
        }
        let mut next = current.clone();
        next.dismissed_release = Some(WorkflowRelease {
            tag: dismissed.tag.clone(),
            version: dismissed.version.clone(),
            commit: dismissed.commit.clone(),
        });
        self.store.write_locked_receipt(&locked, &current, &next)?;
        Ok(ManagedWorkflowRecord::from(next))
    }
}
