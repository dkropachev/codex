use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_file_system::ExecutorFileSystem;
use codex_file_system::RemoveOptions;
use codex_utils_path_uri::PathUri;

pub(super) const TEMP_FILE_PREFIX: &str = ".codex-review-commit";
static NEXT_TEMP_FILE_ID: AtomicU64 = AtomicU64::new(/*v*/ 0);

pub(super) struct TemporaryGitArtifacts {
    pub(super) paths: Vec<PathUri>,
}

impl TemporaryGitArtifacts {
    pub(super) fn new(index_path: &PathUri, roles: &[&str]) -> Result<Self> {
        let parent = index_path
            .parent()
            .context("Git index path has no parent directory")?;
        let sequence = NEXT_TEMP_FILE_ID.fetch_add(/*val*/ 1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let nonce = format!("{}-{timestamp}-{sequence}", std::process::id());
        let mut paths = Vec::with_capacity(roles.len());
        for role in roles {
            paths.push(
                parent
                    .join(&format!("{TEMP_FILE_PREFIX}-{nonce}-{role}"))
                    .with_context(|| format!("failed to create temporary {role} path"))?,
            );
        }
        Ok(Self { paths })
    }

    pub(super) async fn cleanup(&self, fs: &dyn ExecutorFileSystem) -> Result<()> {
        let mut failures = Vec::new();
        for path in &self.paths {
            remove_file(fs, path, &mut failures).await;
            let lock_path = path
                .parent()
                .and_then(|parent| {
                    path.basename()
                        .map(|basename| (parent, format!("{basename}.lock")))
                })
                .and_then(|(parent, basename)| parent.join(&basename).ok());
            if let Some(lock_path) = lock_path {
                remove_file(fs, &lock_path, &mut failures).await;
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!(
                "failed to clean temporary Git files: {}",
                failures.join("; ")
            )
        }
    }
}

async fn remove_file(fs: &dyn ExecutorFileSystem, path: &PathUri, failures: &mut Vec<String>) {
    if let Err(err) = fs
        .remove(
            path,
            RemoveOptions {
                recursive: false,
                force: true,
            },
            /*sandbox*/ None,
        )
        .await
    {
        failures.push(format!("{path}: {err}"));
    }
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
