use std::fs;
use std::io::ErrorKind;
use std::io::Write;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::super::ValidatedDependencySources;
use super::super::bun::ManagedBunEnvironment;
use super::super::lockfile::MAX_BUN_LOCK_BYTES;
use super::super::lockfile::validate_text_lock_contents;
use super::file::read_bounded_regular_file;
use super::file::validate_directory_component;

/// Copies committed binary-lock inputs into an empty operation-private scratch directory.
///
/// The caller must provide a previously materialized managed Bun environment whose scratch
/// directory is dedicated to this operation, keep the candidate outside that environment, and
/// keep both already-validated directory trees stable until staging completes.
pub(super) struct StagedBinaryLockInputs {
    package_json: Vec<u8>,
    binary_lock: Vec<u8>,
}

pub(super) fn stage_binary_lock_inputs(
    candidate: &AbsolutePathBuf,
    environment: &ManagedBunEnvironment,
) -> anyhow::Result<StagedBinaryLockInputs> {
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
    ])?;
    Ok(StagedBinaryLockInputs {
        package_json,
        binary_lock,
    })
}

/// Checks the complete inspection scratch before the retained frozen install may run.
pub(super) fn validate_binary_lock_inspection(
    staged: &StagedBinaryLockInputs,
    environment: &ManagedBunEnvironment,
    package: &crate::WorkflowPackage,
    sources: &ValidatedDependencySources,
) -> anyhow::Result<()> {
    let scratch = environment.scratch_dir.as_path();
    let metadata = fs::symlink_metadata(scratch)
        .with_context(|| format!("failed to inspect {}", scratch.display()))?;
    validate_directory_component(scratch, &metadata)?;

    let mut entries = std::collections::BTreeSet::new();
    for entry in fs::read_dir(scratch).context("failed to inspect managed Bun scratch directory")? {
        let entry = entry.context("failed to inspect managed Bun scratch entry")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            bail!("managed Bun inspection created a non-UTF-8 scratch entry");
        };
        if !matches!(name, "package.json" | "bun.lockb" | "bun.lock") {
            bail!("managed Bun inspection created an unexpected scratch entry `{name}`");
        }
        entries.insert(name.to_owned());
    }
    if entries
        != std::collections::BTreeSet::from([
            "package.json".to_owned(),
            "bun.lockb".to_owned(),
            "bun.lock".to_owned(),
        ])
    {
        bail!("managed Bun inspection did not produce exactly the required scratch files");
    }

    for (name, expected, limit) in [
        (
            "package.json",
            staged.package_json.as_slice(),
            crate::manifest::MAX_PACKAGE_JSON_BYTES,
        ),
        (
            "bun.lockb",
            staged.binary_lock.as_slice(),
            MAX_BUN_LOCK_BYTES,
        ),
    ] {
        let actual = read_bounded_regular_file(&environment.scratch_dir.join(name), limit)?;
        if actual != expected {
            bail!("managed Bun inspection changed staged {name}");
        }
    }
    // The bounded read also rejects aliases and special files before JSONC parsing.
    let lock_path = environment.scratch_dir.join("bun.lock");
    let contents = read_bounded_regular_file(&lock_path, MAX_BUN_LOCK_BYTES)?;
    let contents = std::str::from_utf8(&contents).context("generated bun.lock is not UTF-8")?;
    validate_text_lock_contents(package, sources, contents)
        .context("managed Bun inspection generated an invalid bun.lock")
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
