use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct StageRootGuard {
    path: PathBuf,
}

impl StageRootGuard {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for StageRootGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(crate) fn create_stage_root(base_dir: &Path) -> Result<PathBuf> {
    let stage_root = base_dir.join(".workflow-staging").join(unique_stage_name());
    fs::create_dir_all(&stage_root).with_context(|| {
        format!(
            "failed to create workflow staging root {}",
            stage_root.display()
        )
    })?;
    Ok(stage_root)
}

pub(crate) fn session_stage_root_path(base_dir: &Path, session_id: &str) -> PathBuf {
    base_dir
        .join(".workflow-staging")
        .join("sessions")
        .join(session_id)
}

pub(crate) fn create_session_stage_root(base_dir: &Path, session_id: &str) -> Result<PathBuf> {
    let stage_root = session_stage_root_path(base_dir, session_id);
    fs::create_dir_all(&stage_root).with_context(|| {
        format!(
            "failed to create workflow session staging root {}",
            stage_root.display()
        )
    })?;
    Ok(stage_root)
}

pub(crate) fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create workflow staging dir {}", target.display()))?;

    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
            continue;
        }

        if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
            continue;
        }

        if file_type.is_symlink() {
            copy_symlink(&source_path, &target_path)?;
        }
    }

    Ok(())
}

pub(crate) fn publish_staged_workflow(
    stage_root: &Path,
    staged_path: &Path,
    live_path: &Path,
) -> Result<()> {
    if let Some(parent) = live_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create workflow parent {}", parent.display()))?;
    }

    if !live_path.exists() {
        fs::rename(staged_path, live_path).with_context(|| {
            format!(
                "failed to publish staged workflow {} to {}",
                staged_path.display(),
                live_path.display()
            )
        })?;
        return Ok(());
    }

    let backup_path = stage_root.join(".live-backup");
    if backup_path.exists() {
        fs::remove_dir_all(&backup_path)
            .with_context(|| format!("failed to clear old backup {}", backup_path.display()))?;
    }

    fs::rename(live_path, &backup_path).with_context(|| {
        format!(
            "failed to move live workflow {} aside before publish",
            live_path.display()
        )
    })?;

    match fs::rename(staged_path, live_path) {
        Ok(()) => {
            let _ = fs::remove_dir_all(&backup_path);
            Ok(())
        }
        Err(err) => {
            let restore_result = fs::rename(&backup_path, live_path);
            if let Err(restore_err) = restore_result {
                return Err(anyhow!(
                    "failed to publish staged workflow {} to {}: {err}; additionally failed to restore live workflow from {}: {restore_err}",
                    staged_path.display(),
                    live_path.display(),
                    backup_path.display()
                ));
            }
            Err(anyhow!(
                "failed to publish staged workflow {} to {}: {err}",
                staged_path.display(),
                live_path.display()
            ))
        }
    }
}

fn unique_stage_name() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = STAGING_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("stage-{}-{}-{}", std::process::id(), now, sequence)
}

#[cfg(unix)]
fn copy_symlink(source: &Path, target: &Path) -> Result<()> {
    std::os::unix::fs::symlink(
        fs::read_link(source)
            .with_context(|| format!("failed to read symlink target for {}", source.display()))?,
        target,
    )
    .with_context(|| format!("failed to copy symlink {}", source.display()))
}

#[cfg(windows)]
fn copy_symlink(source: &Path, target: &Path) -> Result<()> {
    let target_value = fs::read_link(source)
        .with_context(|| format!("failed to read symlink target for {}", source.display()))?;
    let metadata = fs::metadata(source)
        .with_context(|| format!("failed to inspect symlink target for {}", source.display()))?;
    if metadata.is_dir() {
        std::os::windows::fs::symlink_dir(&target_value, target)
            .with_context(|| format!("failed to copy directory symlink {}", source.display()))
    } else {
        std::os::windows::fs::symlink_file(&target_value, target)
            .with_context(|| format!("failed to copy file symlink {}", source.display()))
    }
}
