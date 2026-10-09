use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;

use super::ManagedWorkflowDependencyRuntime;
use super::ManagedWorkflowInstallation;
use super::ManagedWorkflowService;
use super::update::ManagedWorkflowUpdate;
use super::update::WorkflowReleaseIdentity;
use crate::managed::ResolvedWorkflowRelease;
use crate::managed::WorkflowGitSource;
use crate::managed::fetch;
use crate::managed::integrity;
use crate::managed::store::ExpectedCurrent;
use crate::managed::store::ManagedWorkflowCommitOutcome;
use crate::managed::store::ReceiptIdentity;
use crate::managed::store::WorkflowRelease;

/// Inputs for explicitly replacing one installed managed workflow release.
pub struct ManagedWorkflowUpdateRequest<'a> {
    pub id: &'a str,
    pub expected_installed: &'a WorkflowReleaseIdentity,
    pub expected_available: &'a WorkflowReleaseIdentity,
    pub dependency_runtime: Option<ManagedWorkflowDependencyRuntime<'a>>,
    pub cancelled: &'a AtomicBool,
}

impl ManagedWorkflowService {
    /// Installs the exact latest eligible release, including one previously dismissed.
    pub fn update(
        &self,
        request: ManagedWorkflowUpdateRequest<'_>,
    ) -> anyhow::Result<ManagedWorkflowInstallation> {
        let ManagedWorkflowUpdateRequest {
            id,
            expected_installed,
            expected_available,
            dependency_runtime,
            cancelled,
        } = request;
        let locked = self.store.lock_install(id, Some(cancelled))?;
        let previous = self
            .store
            .read_locked_verified_receipt(&locked, cancelled)?;
        if WorkflowReleaseIdentity::from(&previous.installed) != *expected_installed {
            bail!("managed workflow release changed before update");
        }
        let ManagedWorkflowUpdate::Available {
            release: available, ..
        } = super::update::check_release(&previous, cancelled)?
        else {
            bail!("managed workflow has no eligible update");
        };
        if &available != expected_available {
            bail!("managed workflow available release changed before update");
        }
        let source = WorkflowGitSource::parse(&previous.source)
            .context("managed workflow source is unavailable")?;
        let release = ResolvedWorkflowRelease {
            tag: available.tag.clone(),
            version: available
                .version
                .as_deref()
                .map(semver::Version::parse)
                .transpose()?,
            advertised_object_id: available.commit,
        };
        let staged = fetch::stage_resolved_workflow_release_cancellable(
            &self.fetch_root,
            &source,
            &release,
            cancelled,
        )?;
        let package = crate::WorkflowPackage::load(staged.root().as_path())?;
        if package.manifest.id != id {
            bail!("workflow update changed the installed manifest ID");
        }
        let deadline =
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 300));
        self.materialize_dependencies(&package, &staged, dependency_runtime, deadline, cancelled)?;
        let verified = integrity::verify_post_install(
            staged,
            fetch::VERIFICATION_LIMITS,
            deadline,
            Some(cancelled),
        )?;
        let mut next = previous.clone();
        next.installed = WorkflowRelease {
            tag: release.tag.clone(),
            version: release.version.as_ref().map(ToString::to_string),
            commit: release.advertised_object_id.clone(),
        };
        next.dismissed_release = None;
        let prepared = self.store.prepare_release(
            &locked,
            verified,
            Some(previous.clone()),
            next,
            fetch::VERIFICATION_LIMITS,
            deadline,
            Some(cancelled),
        )?;
        let expected = ExpectedCurrent::Receipt(ReceiptIdentity::from_receipt(&previous)?);
        let outcome = self.store.commit_replace(&locked, &expected, prepared)?;
        let cleanup_pending = match outcome {
            ManagedWorkflowCommitOutcome::Committed => false,
            ManagedWorkflowCommitOutcome::CommittedCleanupPending => true,
            ManagedWorkflowCommitOutcome::RolledBack
            | ManagedWorkflowCommitOutcome::RolledBackCleanupPending => {
                bail!("managed workflow update rolled back");
            }
        };
        Ok(ManagedWorkflowInstallation {
            id: id.to_owned(),
            source: previous.source,
            release,
            cleanup_pending,
        })
    }
}

#[cfg(test)]
#[path = "update_install_tests.rs"]
mod tests;
