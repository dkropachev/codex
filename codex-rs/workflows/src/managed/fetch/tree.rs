use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::Context;
use anyhow::bail;

#[derive(Clone, Copy)]
struct TreeLimits {
    files: usize,
    bytes: u64,
}

const TREE_LIMITS: TreeLimits = TreeLimits {
    files: 4_096,
    bytes: 128 * 1024 * 1024,
};

const MAX_PATH_BYTES: usize = 4_096;

pub(super) fn verify_tracked_tree(
    git: &OsStr,
    working_directory: &Path,
    repository: &Path,
    commit: &str,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut command = super::repository_command(git, working_directory, repository);
    command
        .args(["ls-tree", "-r", "-z", "-l", "--full-tree"])
        .arg(commit)
        .arg("--");
    let output = super::run_git(command, "Git workflow tree inspection", cancelled)?;
    validate_tree_listing(&output, commit.len(), TREE_LIMITS, cancelled)
}

fn validate_tree_listing(
    output: &[u8],
    object_id_bytes: usize,
    limits: TreeLimits,
    cancelled: Option<&AtomicBool>,
) -> anyhow::Result<()> {
    let mut files = 0_usize;
    let mut bytes = 0_u64;
    let mut portable_paths = BTreeSet::new();
    if output.is_empty() {
        return Ok(());
    }
    if output.last() != Some(&0) {
        bail!("Git returned unterminated workflow tree data");
    }
    for record in output
        .strip_suffix(&[0])
        .unwrap_or(output)
        .split(|byte| *byte == 0)
    {
        if record.is_empty() {
            bail!("Git returned empty workflow tree data");
        }
        super::ensure_not_cancelled(cancelled)?;
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("Git returned malformed workflow tree data")?;
        let (metadata, path) = record.split_at(separator);
        let mut fields = metadata
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let (Some(mode), Some(kind), Some(object_id), Some(size), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            bail!("Git returned malformed workflow tree metadata");
        };
        if kind != b"blob" || !matches!(mode, b"100644" | b"100755") {
            bail!("workflow release contains a symbolic link, submodule, or special entry");
        }
        if object_id.len() != object_id_bytes
            || !object_id.iter().all(u8::is_ascii_hexdigit)
            || object_id.iter().all(|byte| *byte == b'0')
        {
            bail!("workflow release contains an invalid Git object ID");
        }
        let size = std::str::from_utf8(size)
            .context("Git returned a non-UTF-8 workflow file size")?
            .parse::<u64>()
            .context("Git returned an invalid workflow file size")?;
        let path =
            std::str::from_utf8(&path[1..]).context("workflow release paths must be UTF-8")?;
        let portable = portable_path(path)?;
        if is_mutable_runtime_path(&portable) && path != "state/.gitkeep" {
            bail!("workflow release tracks generated or runtime path `{path}`");
        }
        if !portable_paths.insert(portable) {
            bail!("workflow release contains paths that collide on supported platforms");
        }
        files += 1;
        if files > limits.files {
            bail!("workflow release tracks more than {} files", limits.files);
        }
        bytes = bytes
            .checked_add(size)
            .context("workflow tracked file size overflow")?;
        if bytes > limits.bytes {
            bail!("workflow release tracks more than {} bytes", limits.bytes);
        }
    }
    Ok(())
}

pub(in crate::managed) fn portable_path(path: &str) -> anyhow::Result<String> {
    if path.is_empty()
        || !path.is_ascii()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.len() >= MAX_PATH_BYTES
    {
        bail!("workflow release contains an unsafe path");
    }
    let mut portable = Vec::new();
    for component in path.split('/') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.eq_ignore_ascii_case(".git")
            || component.to_ascii_lowercase().starts_with("git~")
            || component.ends_with([' ', '.'])
            || component.len() > 255
            || component
                .bytes()
                .any(|byte| byte < b' ' || byte == 0x7f || b"<>:\"\\|?*".contains(&byte))
            || is_windows_device_name(component)
        {
            bail!("workflow release contains an unsafe path");
        }
        portable.push(component.to_ascii_lowercase());
    }
    Ok(portable.join("/"))
}

fn is_windows_device_name(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
}

fn is_mutable_runtime_path(path: &str) -> bool {
    path.split('/')
        .next()
        .is_some_and(|component| matches!(component, "state" | "artifacts" | "node_modules"))
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
