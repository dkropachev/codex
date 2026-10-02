use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;
use codex_utils_absolute_path::AbsolutePathBuf;

pub(super) fn ensure_candidate_outside_environment(
    candidate: &AbsolutePathBuf,
    cache: &AbsolutePathBuf,
    operation: &Path,
) -> anyhow::Result<()> {
    let operation = absolute_from_path(operation)?;
    if paths_overlap(candidate, cache)? || paths_overlap(candidate, &operation)? {
        bail!("managed Bun environment and workflow candidate must not overlap");
    }
    Ok(())
}

fn paths_overlap(left: &AbsolutePathBuf, right: &AbsolutePathBuf) -> anyhow::Result<bool> {
    let left = fs::canonicalize(left.as_path()).with_context(|| {
        format!(
            "failed to resolve managed path {}",
            left.as_path().display()
        )
    })?;
    let right = fs::canonicalize(right.as_path()).with_context(|| {
        format!(
            "failed to resolve managed path {}",
            right.as_path().display()
        )
    })?;
    Ok(left.starts_with(&right) || right.starts_with(&left))
}

fn absolute_from_path(path: &Path) -> anyhow::Result<AbsolutePathBuf> {
    AbsolutePathBuf::from_absolute_path_checked(path).context("managed path was not absolute")
}
