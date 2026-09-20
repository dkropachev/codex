//! Safe commit creation for verified fixes produced by an isolated review agent.

use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_file_system::ExecutorFileSystem;
use codex_utils_path_uri::PathUri;

use crate::ReviewCommandRunner;

mod artifacts;
mod change;
mod git;
mod index_flags;
mod prepare;
mod transaction;

#[cfg(test)]
use artifacts::TEMP_FILE_PREFIX;
use artifacts::TemporaryGitArtifacts;
use artifacts::combine_with_cleanup;
pub use change::ReviewFixFileChange;
use change::validate_changes;
use git::ensure_head;
use git::resolve_empty_tree;
use git::resolve_head_ref;
use git::resolve_index_path;
use git::resolve_object;
use git::write_tree;
use index_flags::PreservedIndexFlags;
use index_flags::read_preserved_index_flags;
use prepare::prepare_commit_trees;
use transaction::create_commit;
use transaction::finish_commit_transaction;

/// Repository state captured immediately before an isolated fix stage starts.
///
/// The snapshot is opaque because its tree IDs and raw index contents must stay
/// consistent. Pass it unchanged to [`commit_review_fixes`].
#[derive(Clone)]
pub struct ReviewFixCommitSnapshot {
    repository_root: PathUri,
    head_sha: String,
    head_tree: String,
    head_ref: Option<String>,
    index_path: PathUri,
    index_contents: Vec<u8>,
    index_flags: PreservedIndexFlags,
    index_tree: String,
    empty_tree: String,
}

/// Result of attempting to commit exact changes made by a review-fix stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewFixCommitOutcome {
    /// The supplied changes have no net effect relative to `HEAD`.
    NoChanges,
    /// A new commit was created and installed at `HEAD`.
    Committed { commit_sha: String },
}

/// Captures `HEAD` and the raw index before fixes run.
///
/// This does not read or stage worktree content. Git commands and filesystem
/// access run in the executor that owns `repository_root`.
pub async fn capture_review_fix_commit_snapshot(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    repository_root: &PathUri,
) -> Result<ReviewFixCommitSnapshot> {
    let head_sha = resolve_object(
        runner,
        repository_root,
        /*index_path*/ None,
        "HEAD^{commit}",
        "HEAD commit",
    )
    .await?;
    let head_tree = resolve_object(
        runner,
        repository_root,
        /*index_path*/ None,
        "HEAD^{tree}",
        "HEAD tree",
    )
    .await?;
    let head_ref = resolve_head_ref(runner, repository_root).await?;
    let index_path = resolve_index_path(runner, repository_root).await?;
    let index_contents = fs
        .read_file(&index_path, /*sandbox*/ None)
        .await
        .with_context(|| format!("failed to read Git index {index_path}"))?;
    let index_flags = read_preserved_index_flags(runner, repository_root, &index_path).await?;
    let empty_tree = resolve_empty_tree(runner, repository_root).await?;
    let artifacts = TemporaryGitArtifacts::new(&index_path, &["snapshot-index"])?;
    let result = capture_index_tree(
        runner,
        fs,
        repository_root,
        &artifacts.paths[0],
        &index_contents,
        &empty_tree,
    )
    .await;
    let cleanup = artifacts.cleanup(fs).await;
    let index_tree = combine_with_cleanup(result, cleanup)?;
    ensure_head(runner, repository_root, &head_sha, head_ref.as_deref()).await?;
    ensure_index_unchanged(fs, &index_path, &index_contents).await?;

    Ok(ReviewFixCommitSnapshot {
        repository_root: repository_root.clone(),
        head_sha,
        head_tree,
        head_ref,
        index_path,
        index_contents,
        index_flags,
        index_tree,
        empty_tree,
    })
}

/// Returns whether the exact completed changes produce a commit relative to `HEAD`.
///
/// This never reads the worktree or changes a ref or the real index.
pub async fn review_fix_snapshot_has_changes(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    snapshot: &ReviewFixCommitSnapshot,
    changes: &[ReviewFixFileChange],
) -> Result<bool> {
    if changes.is_empty() {
        return Ok(false);
    }
    let changes = validate_changes(&snapshot.repository_root, changes)?;
    ensure_snapshot_git_state(runner, fs, snapshot).await?;
    let artifacts = TemporaryGitArtifacts::new(
        &snapshot.index_path,
        &["commit-index", "preserved-index", "change-content"],
    )?;
    let result = prepare_commit_trees(runner, fs, snapshot, &artifacts.paths, &changes).await;
    let cleanup = artifacts.cleanup(fs).await;
    let prepared = combine_with_cleanup(result, cleanup)?;
    ensure_snapshot_git_state(runner, fs, snapshot).await?;
    Ok(prepared.is_some())
}

/// Creates one focused commit from exact successful `apply_patch` changes.
///
/// Existing staged changes are preserved in the real index. Unstaged changes
/// and edits made after an `apply_patch` record are never read. Every Git
/// command disables hooks and fsmonitor helpers. This function never amends or
/// pushes.
///
/// The final index/ref transaction runs in a detached task. If the caller is
/// cancelled after that transaction starts, it still completes or rolls back.
pub async fn commit_review_fixes<R>(
    runner: Arc<R>,
    fs: Arc<dyn ExecutorFileSystem>,
    snapshot: &ReviewFixCommitSnapshot,
    changes: &[ReviewFixFileChange],
    commit_message: &str,
) -> Result<ReviewFixCommitOutcome>
where
    R: ReviewCommandRunner + 'static,
{
    if commit_message.trim().is_empty() {
        bail!("review fix commit message must not be empty");
    }
    if commit_message.contains('\0') {
        bail!("review fix commit message must not contain a null byte");
    }
    if changes.is_empty() {
        bail!("review fix commit requires at least one exact file change");
    }
    let changes = validate_changes(&snapshot.repository_root, changes)?;
    ensure_snapshot_git_state(runner.as_ref(), fs.as_ref(), snapshot).await?;
    let artifacts = TemporaryGitArtifacts::new(
        &snapshot.index_path,
        &["commit-index", "preserved-index", "change-content"],
    )?;
    let prepared = prepare_commit_trees(
        runner.as_ref(),
        fs.as_ref(),
        snapshot,
        &artifacts.paths,
        &changes,
    )
    .await;
    let prepared = match prepared {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            let cleanup = artifacts.cleanup(fs.as_ref()).await;
            combine_with_cleanup(Ok(()), cleanup)?;
            return Ok(ReviewFixCommitOutcome::NoChanges);
        }
        Err(error) => {
            return combine_with_cleanup(Err(error), artifacts.cleanup(fs.as_ref()).await);
        }
    };
    let commit_sha = create_commit(
        runner.as_ref(),
        snapshot,
        &prepared.commit_tree,
        commit_message,
    )
    .await;
    let commit_sha = match commit_sha {
        Ok(commit_sha) => commit_sha,
        Err(error) => {
            return combine_with_cleanup(Err(error), artifacts.cleanup(fs.as_ref()).await);
        }
    };

    let snapshot = snapshot.clone();
    let transaction = tokio::spawn(async move {
        let result = finish_commit_transaction(
            runner.as_ref(),
            fs.as_ref(),
            &snapshot,
            &prepared.index_mutations,
            &prepared.preserved_index_tree,
            &commit_sha,
        )
        .await;
        combine_with_cleanup(result, artifacts.cleanup(fs.as_ref()).await)
    });
    transaction
        .await
        .context("review fix commit transaction task failed")?
}

async fn capture_index_tree(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    repository_root: &PathUri,
    temporary_index: &PathUri,
    index_contents: &[u8],
    empty_tree: &str,
) -> Result<String> {
    fs.write_file(
        temporary_index,
        index_contents.to_vec(),
        /*sandbox*/ None,
    )
    .await
    .with_context(|| format!("failed to create temporary Git index {temporary_index}"))?;
    write_tree(runner, repository_root, temporary_index, empty_tree).await
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
        bail!("Git index changed after the review fix snapshot");
    }
    Ok(())
}

async fn ensure_snapshot_git_state(
    runner: &impl ReviewCommandRunner,
    fs: &dyn ExecutorFileSystem,
    snapshot: &ReviewFixCommitSnapshot,
) -> Result<()> {
    ensure_head(
        runner,
        &snapshot.repository_root,
        &snapshot.head_sha,
        snapshot.head_ref.as_deref(),
    )
    .await?;
    ensure_index_unchanged(fs, &snapshot.index_path, &snapshot.index_contents).await
}

#[cfg(test)]
#[path = "review_commit_tests.rs"]
mod tests;
