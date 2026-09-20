//! Immutable Git state captured before an isolated review-fix stage.

use std::future::Future;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_file_system::ExecutorFileSystem;
use codex_utils_path_uri::PathUri;

use crate::ReviewCommand;
use crate::ReviewCommandOutput;

mod artifacts;
mod git;

#[cfg(test)]
use artifacts::TEMP_FILE_PREFIX;
use artifacts::TemporaryGitArtifact;
use artifacts::combine_with_cleanup;
use git::ensure_head;
use git::resolve_empty_tree;
use git::resolve_head_ref;
use git::resolve_index_path;
use git::resolve_object;
use git::write_tree;

/// Executes Git snapshot commands in the environment that owns the repository.
///
/// Implementations must enforce command bounds, reject non-UTF-8 output, clear
/// repository-routing `GIT_*` variables, apply explicit overrides, and return
/// only after the command exited or its termination was confirmed.
pub trait ReviewSnapshotCommandRunner: Send + Sync {
    fn run(
        &self,
        command: ReviewCommand,
    ) -> impl Future<Output = Result<ReviewCommandOutput>> + Send;
}

/// Repository state captured immediately before an isolated fix stage starts.
///
/// The opaque value keeps its HEAD and raw-index observations together for
/// later exact-change verification and commit layers.
#[derive(Clone)]
#[allow(dead_code)] // Opaque fields are consumed by the later exact-commit stage.
pub struct ReviewFixCommitSnapshot {
    repository_root: PathUri,
    head_sha: String,
    head_tree: String,
    head_ref: Option<String>,
    index_contents: Vec<u8>,
    index_tree: String,
}

/// Captures `HEAD` and the raw index before fixes run.
///
/// This does not read or stage worktree content. Git commands and filesystem
/// access run in the executor that owns `repository_root`.
pub async fn capture_review_fix_commit_snapshot<R>(
    runner: Arc<R>,
    fs: Arc<dyn ExecutorFileSystem>,
    repository_root: &PathUri,
) -> Result<ReviewFixCommitSnapshot>
where
    R: ReviewSnapshotCommandRunner + 'static,
{
    let repository_root = repository_root.clone();
    tokio::spawn(async move {
        let runner = runner.as_ref();
        let repository_root = &repository_root;
        let head_sha =
            resolve_object(runner, repository_root, "HEAD^{commit}", "HEAD commit").await?;
        let head_tree_revision = format!("{head_sha}^{{tree}}");
        let head_tree =
            resolve_object(runner, repository_root, &head_tree_revision, "HEAD tree").await?;
        let head_ref = resolve_head_ref(runner, repository_root).await?;
        let index_path = resolve_index_path(runner, repository_root).await?;
        let index_contents = fs
            .read_file(&index_path, /*sandbox*/ None)
            .await
            .with_context(|| format!("failed to read Git index {index_path}"))?;
        let empty_tree = resolve_empty_tree(runner, repository_root).await?;
        let artifact = TemporaryGitArtifact::new(&index_path, "snapshot-index")?;
        let temporary_index = &artifact.path;
        let capture = async {
            fs.write_file(
                temporary_index,
                index_contents.clone(),
                /*sandbox*/ None,
            )
            .await
            .with_context(|| format!("failed to create temporary Git index {temporary_index}"))?;
            write_tree(runner, repository_root, temporary_index, &empty_tree).await
        }
        .await;
        let index_tree = combine_with_cleanup(capture, artifact.cleanup(fs.as_ref()).await)?;
        ensure_head(runner, repository_root, &head_sha, head_ref.as_deref()).await?;
        ensure_index_unchanged(fs.as_ref(), &index_path, &index_contents).await?;

        Ok(ReviewFixCommitSnapshot {
            repository_root: (*repository_root).clone(),
            head_sha,
            head_tree,
            head_ref,
            index_contents,
            index_tree,
        })
    })
    .await
    .context("review fix snapshot task failed")?
}

async fn ensure_index_unchanged(
    fs: &dyn ExecutorFileSystem,
    index_path: &PathUri,
    expected_contents: &[u8],
) -> Result<()> {
    let actual_contents = fs
        .read_file(index_path, /*sandbox*/ None)
        .await
        .with_context(|| format!("failed to verify Git index {index_path}"))?;
    if actual_contents != expected_contents {
        bail!("Git index changed while capturing the review fix snapshot");
    }
    Ok(())
}

#[cfg(test)]
#[path = "review_commit_tests.rs"]
mod tests;
