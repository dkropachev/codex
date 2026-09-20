use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathUri;

use super::git::run_git_checked;
use super::git::run_git_dynamic_checked;
use crate::ReviewCommandRunner;
use crate::pull_request::REVIEW_COMMAND_OUTPUT_BYTES_CAP;

#[derive(Clone, Default)]
pub(super) struct PreservedIndexFlags {
    assume_unchanged: Vec<String>,
    skip_worktree: Vec<String>,
}

pub(super) async fn read_preserved_index_flags(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
) -> Result<PreservedIndexFlags> {
    let output = run_git_checked(
        runner,
        repository_root,
        Some(index_path),
        ["ls-files", "-v", "-z"],
        "git ls-files",
    )
    .await?;
    if output.stdout.len() >= REVIEW_COMMAND_OUTPUT_BYTES_CAP {
        bail!("Git index is too large to preserve review fix flags safely");
    }

    let mut flags = PreservedIndexFlags::default();
    for record in output.stdout.split_terminator('\0') {
        let bytes = record.as_bytes();
        if bytes.is_empty() {
            continue;
        }
        if bytes.len() < 3 || bytes[1] != b' ' {
            bail!("git ls-files returned invalid flag output");
        }
        match bytes[0] {
            b'h' => flags.assume_unchanged.push(parse_flagged_path(record)?),
            b'S' => flags.skip_worktree.push(parse_flagged_path(record)?),
            b's' => {
                let path = parse_flagged_path(record)?;
                flags.assume_unchanged.push(path.clone());
                flags.skip_worktree.push(path);
            }
            _ => {}
        }
    }
    Ok(flags)
}

fn parse_flagged_path(record: &str) -> Result<String> {
    let path = &record[2..];
    if path.is_empty() || path.contains('\u{fffd}') {
        bail!("git ls-files returned an unsupported flagged path");
    }
    Ok(path.to_string())
}

pub(super) async fn restore_preserved_index_flags(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    flags: &PreservedIndexFlags,
) -> Result<()> {
    restore_index_flag_group(
        runner,
        repository_root,
        index_path,
        "--assume-unchanged",
        &flags.assume_unchanged,
    )
    .await?;
    restore_index_flag_group(
        runner,
        repository_root,
        index_path,
        "--skip-worktree",
        &flags.skip_worktree,
    )
    .await
}

pub(super) async fn restore_preserved_index_flags_for_paths(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    flags: &PreservedIndexFlags,
    paths: &[String],
) -> Result<()> {
    let assume_unchanged = flags
        .assume_unchanged
        .iter()
        .filter(|path| paths.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    let skip_worktree = flags
        .skip_worktree
        .iter()
        .filter(|path| paths.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    restore_index_flag_group(
        runner,
        repository_root,
        index_path,
        "--assume-unchanged",
        &assume_unchanged,
    )
    .await?;
    restore_index_flag_group(
        runner,
        repository_root,
        index_path,
        "--skip-worktree",
        &skip_worktree,
    )
    .await
}

async fn restore_index_flag_group(
    runner: &impl ReviewCommandRunner,
    repository_root: &PathUri,
    index_path: &PathUri,
    flag: &str,
    paths: &[String],
) -> Result<()> {
    const PATH_BYTES_PER_COMMAND: usize = 16 * 1024;
    let mut start = 0;
    while start < paths.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < paths.len()
            && (end == start || bytes + paths[end].len() <= PATH_BYTES_PER_COMMAND)
        {
            bytes += paths[end].len();
            end += 1;
        }
        let args = [
            "update-index".to_string(),
            flag.to_string(),
            "--".to_string(),
        ]
        .into_iter()
        .chain(paths[start..end].iter().cloned())
        .collect();
        run_git_dynamic_checked(
            runner,
            repository_root,
            Some(index_path),
            args,
            "failed to restore Git index flags",
        )
        .await?;
        start = end;
    }
    Ok(())
}
