use std::sync::atomic::AtomicBool;

mod dependencies;
mod fetch;
mod git_command;
#[allow(
    dead_code,
    reason = "consumed by post-install verification in the next slice"
)]
mod integrity;
mod jsonc;
mod management;
mod release;
mod source;
#[allow(dead_code, reason = "used by managed lifecycle stages")]
mod store;

pub use management::ManagedWorkflowDependencyRuntime;
pub use management::ManagedWorkflowInstallRequest;
pub use management::ManagedWorkflowInstallation;
pub use management::ManagedWorkflowRecord;
pub use management::ManagedWorkflowService;
pub use management::ManagedWorkflowUpdate;
pub use management::ManagedWorkflowUpdateCheck;
pub use management::WorkflowReleaseIdentity;
pub use management::WorkflowUpdatePolicy;
pub use release::ResolvedWorkflowRelease;
pub use source::WorkflowGitSource;

/// Resolves the highest stable SemVer release advertised by a Git source.
///
/// Repositories without a stable release tag resolve to their current `HEAD`
/// object. Callers must fetch the selected ref and verify that the advertised
/// object resolves to a commit before trusting it.
pub fn resolve_workflow_git_release(
    source: &WorkflowGitSource,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    git_command::resolve_workflow_git_release(source, /*cancelled*/ None)
}

/// Cancellable form of [`resolve_workflow_git_release`].
pub fn resolve_workflow_git_release_cancellable(
    source: &WorkflowGitSource,
    cancelled: &AtomicBool,
) -> anyhow::Result<ResolvedWorkflowRelease> {
    git_command::resolve_workflow_git_release(source, Some(cancelled))
}
