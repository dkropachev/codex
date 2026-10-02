use std::fs;
use std::io::Write;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::super::bun::ManagedBunEnvironment;
use super::super::lockfile::MAX_BUN_LOCK_BYTES;
use super::file::read_bounded_regular_file;
use super::file::validate_directory_component;

/// Copies committed binary-lock inputs into an empty operation-private scratch directory.
///
/// The caller must provide a previously materialized managed Bun environment whose scratch
/// directory is dedicated to this operation, keep the candidate outside that environment, and
/// prevent concurrent mutation of either tree until staging completes.
pub(super) fn stage_binary_lock_inputs(
    candidate: &AbsolutePathBuf,
    environment: &ManagedBunEnvironment,
) -> anyhow::Result<()> {
    let package_json = read_bounded_regular_file(
        &candidate.join("package.json"),
        crate::manifest::MAX_PACKAGE_JSON_BYTES,
    )?;
    let binary_lock = read_bounded_regular_file(&candidate.join("bun.lockb"), MAX_BUN_LOCK_BYTES)?;

    let scratch = environment.scratch_dir.as_path();
    let metadata = fs::symlink_metadata(scratch)
        .with_context(|| format!("failed to inspect {}", scratch.display()))?;
    validate_directory_component(scratch, &metadata)?;
    if fs::read_dir(scratch)
        .context("failed to inspect managed Bun scratch directory")?
        .next()
        .transpose()
        .context("failed to inspect managed Bun scratch directory")?
        .is_some()
    {
        bail!("managed Bun scratch directory was not empty");
    }

    write_staged_input(&environment.scratch_dir.join("package.json"), &package_json)
        .context("failed to stage package.json for managed binary lockfile conversion")?;
    write_staged_input(&environment.scratch_dir.join("bun.lockb"), &binary_lock)
        .context("failed to stage bun.lockb for managed binary lockfile conversion")
}

fn write_staged_input(path: &AbsolutePathBuf, contents: &[u8]) -> anyhow::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.as_path())?
        .write_all(contents)?;
    Ok(())
}

#[cfg(test)]
#[path = "binary_lock_tests.rs"]
mod tests;
