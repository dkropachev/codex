use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use codex_sandboxing::LocalSandboxRuntime;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::WorkflowGitSource;
use super::dependencies::ManagedDependencyMaterializationOutcome;
use super::dependencies::ManagedDependencyMaterializationRequest;
use super::dependencies::materialize_managed_dependencies;
use super::fetch;
use super::integrity;
use super::store::ExpectedCurrent;
use super::store::ManagedWorkflowCommitOutcome;
use super::store::ManagedWorkflowReceipt;
use super::store::ManagedWorkflowStore;
use super::store::WorkflowRelease;
use super::store::ensure_fresh_target;

#[cfg(any(unix, windows))]
mod policy;
#[cfg(any(unix, windows))]
mod run_workspace;
#[cfg(any(unix, windows))]
mod uninstall;
mod update;
#[cfg(any(unix, windows))]
mod update_install;

#[cfg(any(unix, windows))]
pub use run_workspace::ManagedWorkflowRunWorkspace;
#[cfg(any(unix, windows))]
pub use uninstall::ManagedWorkflowUninstallation;
pub use update::ManagedWorkflowRecord;
pub use update::ManagedWorkflowUpdate;
pub use update::ManagedWorkflowUpdateCheck;
pub use update::WorkflowReleaseIdentity;
pub use update::WorkflowUpdatePolicy;
#[cfg(any(unix, windows))]
pub use update_install::ManagedWorkflowUpdateRequest;

/// Bun and mandatory sandbox inputs for a release that declares dependencies.
pub struct ManagedWorkflowDependencyRuntime<'a> {
    pub bun_executable: &'a AbsolutePathBuf,
    pub sandbox: LocalSandboxRuntime<'a>,
}

/// Inputs for installing one exact release from a local or remote Git source.
pub struct ManagedWorkflowInstallRequest<'a> {
    pub source: &'a str,
    pub dependency_runtime: Option<ManagedWorkflowDependencyRuntime<'a>>,
    pub cancelled: &'a AtomicBool,
}

/// The installed release and whether committed transaction cleanup remains pending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedWorkflowInstallation {
    pub id: String,
    pub source: String,
    pub release: super::ResolvedWorkflowRelease,
    pub cleanup_pending: bool,
}

/// Coordinates verified managed installation for both CLI and app-server callers.
pub struct ManagedWorkflowService {
    store: ManagedWorkflowStore,
    workflow_root: AbsolutePathBuf,
    management_root: AbsolutePathBuf,
    fetch_root: AbsolutePathBuf,
}

impl ManagedWorkflowService {
    /// Opens the shared transaction store and recovers interrupted work first.
    pub fn new(
        codex_home: &AbsolutePathBuf,
        workflow_root: &AbsolutePathBuf,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(workflow_root.as_path())
            .context("failed to create managed workflow root")?;
        let store = ManagedWorkflowStore::create(codex_home, workflow_root)?;
        Ok(Self {
            store,
            workflow_root: workflow_root.clone(),
            management_root: codex_home.join(".workflow-management"),
            fetch_root: codex_home.join(".workflow-fetch"),
        })
    }

    /// Installs the selected release with the default prompt update policy.
    pub fn install(
        &self,
        request: ManagedWorkflowInstallRequest<'_>,
    ) -> anyhow::Result<ManagedWorkflowInstallation> {
        self.install_with_policy(request, WorkflowUpdatePolicy::Prompt)
    }

    /// Installs the selected release with the requested update policy.
    pub fn install_with_policy(
        &self,
        request: ManagedWorkflowInstallRequest<'_>,
        policy: WorkflowUpdatePolicy,
    ) -> anyhow::Result<ManagedWorkflowInstallation> {
        let ManagedWorkflowInstallRequest {
            source,
            dependency_runtime,
            cancelled,
        } = request;
        let source = WorkflowGitSource::parse(source)?;
        let release = super::git_command::resolve_workflow_git_release(&source, Some(cancelled))?;
        let staged = fetch::stage_resolved_workflow_release_cancellable(
            &self.fetch_root,
            &source,
            &release,
            cancelled,
        )?;
        let package = crate::WorkflowPackage::load(staged.root().as_path())?;
        let id = package.manifest.id.clone();
        let locked = self.store.lock_install(&id, Some(cancelled))?;
        ensure_fresh_target(&self.store, &locked, &ExpectedCurrent::Absent)?;
        let deadline =
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 300));
        self.materialize_dependencies(&package, &staged, dependency_runtime, deadline, cancelled)?;
        let verified = integrity::verify_post_install(
            staged,
            fetch::VERIFICATION_LIMITS,
            deadline,
            Some(cancelled),
        )?;
        let receipt = ManagedWorkflowReceipt::new(
            id.clone(),
            source.receipt_source()?,
            WorkflowRelease {
                tag: release.tag.clone(),
                version: release.version.as_ref().map(ToString::to_string),
                commit: release.advertised_object_id.clone(),
            },
            policy,
        )?;
        let prepared = self.store.prepare_release(
            &locked,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            fetch::VERIFICATION_LIMITS,
            deadline,
            Some(cancelled),
        )?;
        let outcome = self
            .store
            .commit_fresh(&locked, &ExpectedCurrent::Absent, prepared)?;
        let cleanup_pending = match outcome {
            ManagedWorkflowCommitOutcome::Committed => false,
            ManagedWorkflowCommitOutcome::CommittedCleanupPending => true,
            ManagedWorkflowCommitOutcome::RolledBack
            | ManagedWorkflowCommitOutcome::RolledBackCleanupPending => {
                bail!("managed workflow installation rolled back");
            }
        };
        Ok(ManagedWorkflowInstallation {
            id,
            source: receipt.source,
            release,
            cleanup_pending,
        })
    }

    fn materialize_dependencies(
        &self,
        package: &crate::WorkflowPackage,
        staged: &fetch::StagedWorkflowRelease,
        runtime: Option<ManagedWorkflowDependencyRuntime<'_>>,
        deadline: crate::runner::CommandDeadline,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<()> {
        if !staged.dependencies().sources.has_dependencies {
            return Ok(());
        }
        let runtime =
            runtime.context("managed workflow dependencies require Bun and a local sandbox")?;
        match materialize_managed_dependencies(
            ManagedDependencyMaterializationRequest {
                package,
                dependencies: staged.dependencies(),
                management_root: &self.management_root,
                bun_executable: runtime.bun_executable,
                deadline,
                limits: crate::runner::CommandOutputLimits {
                    stdout_bytes: 64 * 1024,
                    stderr_bytes: 64 * 1024,
                },
                cancelled: Some(cancelled),
            },
            runtime.sandbox,
        )? {
            ManagedDependencyMaterializationOutcome::Materialized => Ok(()),
            ManagedDependencyMaterializationOutcome::SandboxUnavailable(reason) => {
                bail!("managed workflow sandbox is unavailable: {reason:?}")
            }
        }
    }
}

#[cfg(test)]
#[path = "management_tests.rs"]
mod tests;
