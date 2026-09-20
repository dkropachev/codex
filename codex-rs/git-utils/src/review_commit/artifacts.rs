use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use codex_file_system::ExecutorFileSystem;
use codex_file_system::RemoveOptions;
use codex_utils_path_uri::PathUri;

pub(super) const TEMP_FILE_PREFIX: &str = ".codex-review-commit";
static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(/*v*/ 0);

pub(super) struct TemporaryGitArtifact {
    pub(super) path: PathUri,
}

impl TemporaryGitArtifact {
    pub(super) fn new(index_path: &PathUri, role: &str) -> Result<Self> {
        let parent = index_path
            .parent()
            .context("Git index path has no parent directory")?;
        let sequence = NEXT_TEMP_FILE_ID.fetch_add(/*val*/ 1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "{TEMP_FILE_PREFIX}-{}-{timestamp}-{sequence}-{role}",
            std::process::id()
        );
        Ok(Self {
            path: parent
                .join(&name)
                .context("failed to create temporary Git path")?,
        })
    }

    pub(super) async fn cleanup(&self, fs: &dyn ExecutorFileSystem) -> Result<()> {
        cleanup_path(fs, &self.path).await
    }
}

async fn cleanup_path(fs: &dyn ExecutorFileSystem, path: &PathUri) -> Result<()> {
    remove_file(fs, path).await?;
    let parent = path.parent().context("temporary Git path has no parent")?;
    let basename = path
        .basename()
        .context("temporary Git path has no basename")?;
    let lock_path = parent
        .join(&format!("{basename}.lock"))
        .context("failed to create temporary Git lock path")?;
    remove_file(fs, &lock_path).await
}

async fn remove_file(fs: &dyn ExecutorFileSystem, path: &PathUri) -> Result<()> {
    fs.remove(
        path,
        RemoveOptions {
            recursive: false,
            force: true,
        },
        /*sandbox*/ None,
    )
    .await
    .with_context(|| format!("failed to clean temporary Git file {path}"))
}

pub(super) fn combine_with_cleanup<T>(result: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) => Err(err),
        (Ok(_), Err(cleanup_err)) => Err(cleanup_err),
        (Err(err), Err(cleanup_err)) => {
            Err(anyhow::anyhow!("{err:#}; additionally, {cleanup_err:#}"))
        }
    }
}
