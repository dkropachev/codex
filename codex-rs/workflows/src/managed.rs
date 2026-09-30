use std::sync::atomic::AtomicBool;

mod git_command;
mod release;
mod source;

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
