use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::ManagedWorkflowStore;
use super::copy;
use super::fs::SecureDirectory;
use super::journal;
use super::publish;
use super::receipt;
use crate::managed::fetch::VERIFICATION_LIMITS;
use crate::managed::integrity;

pub(super) fn create_run_copy(
    store: &ManagedWorkflowStore,
    id: &str,
    cancelled: &AtomicBool,
) -> anyhow::Result<tempfile::TempDir> {
    if publish::journal_exists(&store.journals, id)? {
        bail!("managed workflow has an unresolved transaction journal");
    }
    let receipt = receipt::read_receipt(&store.receipts, id)?;
    let parent = publish::active_parent(&store.active_root, id, publish::ParentMode::Existing)?
        .context("managed workflow active parent disappeared")?;
    let active = parent.directory().existing_child(publish::leaf(id)?)?;
    let retained = active.identity()?;
    let marker = journal::read_marker(&active)?;
    if marker.id != id || marker.release != receipt.installed {
        bail!("managed workflow active marker does not match its receipt");
    }
    let deadline = crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 300));
    let expected = integrity::published_payload_evidence(
        active.path().as_path(),
        VERIFICATION_LIMITS,
        deadline,
        Some(cancelled),
    )?;
    if expected.sha256 != marker.evidence_digest {
        bail!("managed workflow active payload differs from its marker");
    }
    let runs = store.management.child("runs")?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("run-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(/*mode*/ 0o700));
    }
    let temporary = builder
        .tempdir_in(runs.path().as_path())
        .context("failed to create private workflow run directory")?;
    let operation_root = AbsolutePathBuf::from_absolute_path_checked(temporary.path())?;
    let destination = SecureDirectory::open_root(&operation_root)?;
    let name = temporary
        .path()
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("run workspace directory has no UTF-8 name")?;
    destination.write_file(
        ".codex-managed-run",
        name.as_bytes(),
        /*replace*/ false,
    )?;
    copy::copy_verified_payload(
        active.path(),
        &destination,
        VERIFICATION_LIMITS,
        deadline,
        Some(cancelled),
    )?;
    let copied = integrity::backup_payload_evidence(
        &temporary.path().join("payload"),
        VERIFICATION_LIMITS,
        deadline,
        Some(cancelled),
    )?;
    if copied != expected
        || integrity::published_payload_evidence(
            active.path().as_path(),
            VERIFICATION_LIMITS,
            deadline,
            Some(cancelled),
        )? != expected
        || SecureDirectory::open_root(active.path())?.identity()? != retained
    {
        bail!("managed workflow run copy differs from the installed release");
    }
    Ok(temporary)
}
