use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::ManagedWorkflowService;
use crate::managed::store::ManagedWorkflowRunGuard;

/// A verified private copy retained until managed execution and completion finish.
#[must_use = "dropping the run workspace releases its workflow lock and private copy"]
pub struct ManagedWorkflowRunWorkspace {
    id: String,
    root: AbsolutePathBuf,
    _temporary: tempfile::TempDir,
    _locked: ManagedWorkflowRunGuard,
}

impl ManagedWorkflowRunWorkspace {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn root(&self) -> &Path {
        self.root.as_path()
    }
}

impl ManagedWorkflowService {
    /// Copies a verified installed release into private run storage under its shared lock.
    pub fn prepare_run_workspace(
        &self,
        id: &str,
        selected_path: &Path,
        cancelled: &AtomicBool,
    ) -> anyhow::Result<ManagedWorkflowRunWorkspace> {
        let locked = self.store.lock_run(id, Some(cancelled))?;
        if selected_path != self.workflow_root.join(id).as_path() {
            bail!("managed workflow command path does not match its installed release");
        }
        let temporary = self.store.create_run_copy(&locked, id, cancelled)?;
        let root = AbsolutePathBuf::from_absolute_path_checked(temporary.path().join("payload"))?;
        Ok(ManagedWorkflowRunWorkspace {
            id: id.to_owned(),
            root,
            _temporary: temporary,
            _locked: locked,
        })
    }
}

#[cfg(test)]
#[path = "run_workspace_tests.rs"]
mod tests;
