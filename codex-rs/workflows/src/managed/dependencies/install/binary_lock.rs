use std::fs;
use std::io::ErrorKind;
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
/// keep both already-validated directory trees stable until staging completes.
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

    let package_path = environment.scratch_dir.join("package.json");
    let lock_path = environment.scratch_dir.join("bun.lockb");
    write_staged_inputs(&[
        (package_path, package_json.as_slice()),
        (lock_path, binary_lock.as_slice()),
    ])
}

fn write_staged_inputs(inputs: &[(AbsolutePathBuf, &[u8])]) -> anyhow::Result<()> {
    let mut created = Vec::new();
    for (path, contents) in inputs {
        let result = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;

                options.mode(0o600);
            }
            let mut file = options.open(path.as_path())?;
            created.push(path.clone());
            file.write_all(contents)
        })()
        .with_context(|| {
            format!(
                "failed to stage {} for managed binary lockfile conversion",
                path.as_path().display()
            )
        });
        if let Err(error) = result {
            let mut cleanup_error = None;
            for path in created.iter().rev() {
                match fs::remove_file(path.as_path()) {
                    Ok(()) => {}
                    Err(remove_error) if remove_error.kind() == ErrorKind::NotFound => {}
                    Err(remove_error) => cleanup_error = cleanup_error.or(Some(remove_error)),
                }
            }
            if let Some(cleanup_error) = cleanup_error {
                return Err(anyhow::anyhow!(
                    "{error:#}; staged input cleanup also failed: {cleanup_error:#}"
                ));
            }
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "binary_lock_tests.rs"]
mod tests;
