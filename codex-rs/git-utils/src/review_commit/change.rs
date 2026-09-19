use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;

/// One exact file mutation completed by an `apply_patch` tool call.
///
/// Keep values in completion order. Applying the records never rereads the
/// worktree, so edits made after the tool call cannot enter the review commit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewFixFileChange {
    Add {
        path: PathUri,
        content: String,
    },
    Delete {
        path: PathUri,
        content: String,
    },
    Update {
        path: PathUri,
        unified_diff: String,
        move_path: Option<PathUri>,
    },
}

impl ReviewFixFileChange {
    pub(super) fn path(&self) -> &PathUri {
        match self {
            Self::Add { path, .. } | Self::Delete { path, .. } | Self::Update { path, .. } => path,
        }
    }

    pub(super) fn move_path(&self) -> Option<&PathUri> {
        match self {
            Self::Update { move_path, .. } => move_path.as_ref(),
            Self::Add { .. } | Self::Delete { .. } => None,
        }
    }
}

pub(super) struct ValidatedReviewFixChange<'a> {
    pub(super) change: &'a ReviewFixFileChange,
    pub(super) path: String,
    pub(super) move_path: Option<String>,
}

pub(super) fn validate_changes<'a>(
    repository_root: &PathUri,
    changes: &'a [ReviewFixFileChange],
) -> Result<Vec<ValidatedReviewFixChange<'a>>> {
    changes
        .iter()
        .map(|change| {
            Ok(ValidatedReviewFixChange {
                path: repository_relative_path(repository_root, change.path())?,
                move_path: change
                    .move_path()
                    .map(|path| repository_relative_path(repository_root, path))
                    .transpose()?,
                change,
            })
        })
        .collect()
}

pub(super) fn affected_pathspecs(changes: &[ValidatedReviewFixChange<'_>]) -> Vec<String> {
    let mut paths = changes
        .iter()
        .flat_map(|change| {
            std::iter::once(change.path.clone()).chain(change.move_path.iter().cloned())
        })
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    paths
}

pub(super) fn apply_update_diff(original: &str, unified_diff: &str) -> Result<Option<String>> {
    if unified_diff.is_empty() {
        return Ok(None);
    }
    let hunks = parse_unified_diff(unified_diff)?;
    let mut lines = original.split('\n').map(str::to_string).collect::<Vec<_>>();
    let mut has_final_newline = lines.last().is_some_and(String::is_empty);
    if has_final_newline {
        lines.pop();
    }
    let mut offset = 0isize;
    let mut search_start = 0;
    for hunk in hunks {
        let expected = hunk
            .old_start
            .saturating_sub(1)
            .checked_add_signed(offset)
            .context("review fix update has an invalid line offset")?;
        let position = locate_old_lines(&lines, &hunk.old_lines, expected, search_start)?;
        let old_len = hunk.old_lines.len();
        lines.splice(position..position + old_len, hunk.new_lines.iter().cloned());
        offset += hunk.new_lines.len() as isize - old_len as isize;
        search_start = position + hunk.new_lines.len();
        if hunk.changes_final_newline {
            has_final_newline = hunk.new_has_final_newline;
        }
    }
    let mut result = lines.join("\n");
    if has_final_newline {
        result.push('\n');
    }
    Ok(Some(result))
}

fn repository_relative_path(repository_root: &PathUri, path: &PathUri) -> Result<String> {
    let convention = repository_root
        .infer_path_convention()
        .context("cannot infer the review repository path convention")?;
    let root = repository_root.inferred_native_path_string();
    let absolute = path.inferred_native_path_string();
    if path == repository_root || !path.starts_with(repository_root) {
        bail!("review fix path is outside the repository: {path}");
    }
    let relative = absolute
        .strip_prefix(&root)
        .context("review fix path does not share the repository path spelling")?;
    let root_ends_with_separator = match convention {
        PathConvention::Posix => root.ends_with('/'),
        PathConvention::Windows => root.ends_with('/') || root.ends_with('\\'),
    };
    let relative = if root_ends_with_separator {
        Some(relative)
    } else {
        match convention {
            PathConvention::Posix => relative.strip_prefix('/'),
            PathConvention::Windows => relative
                .strip_prefix('/')
                .or_else(|| relative.strip_prefix('\\')),
        }
    }
    .filter(|relative| !relative.is_empty())
    .with_context(|| format!("review fix path is not a repository file: {path}"))?;
    if relative.contains('\0') {
        bail!("review fix path contains a null byte: {path}");
    }
    Ok(match convention {
        PathConvention::Posix => relative.to_string(),
        PathConvention::Windows => relative.replace('\\', "/"),
    })
}

#[derive(Debug)]
struct UnifiedHunk {
    old_start: usize,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    changes_final_newline: bool,
    new_has_final_newline: bool,
}

fn parse_unified_diff(diff: &str) -> Result<Vec<UnifiedHunk>> {
    if diff.contains('\0') || !diff.ends_with('\n') {
        bail!("review fix update contains an invalid unified diff");
    }
    let lines = diff.split_terminator('\n').collect::<Vec<_>>();
    let mut index = 0;
    let mut hunks = Vec::new();
    while index < lines.len() {
        let (old_start, mut old_remaining, mut new_remaining) = parse_hunk_header(lines[index])?;
        index += 1;
        let mut old_lines = Vec::with_capacity(old_remaining);
        let mut new_lines = Vec::with_capacity(new_remaining);
        let mut changes_final_newline = false;
        let mut new_has_final_newline = true;
        while old_remaining > 0 || new_remaining > 0 {
            let line = lines
                .get(index)
                .context("review fix update has a truncated unified diff")?;
            match line.as_bytes().first() {
                Some(b' ') if old_remaining > 0 && new_remaining > 0 => {
                    old_lines.push(line[1..].to_string());
                    new_lines.push(line[1..].to_string());
                    old_remaining -= 1;
                    new_remaining -= 1;
                }
                Some(b'-') if old_remaining > 0 => {
                    old_lines.push(line[1..].to_string());
                    old_remaining -= 1;
                }
                Some(b'+') if new_remaining > 0 => {
                    new_lines.push(line[1..].to_string());
                    new_remaining -= 1;
                }
                _ => bail!("review fix update contains an invalid unified diff line"),
            }
            index += 1;
            if lines.get(index) == Some(&"\\ No newline at end of file") {
                changes_final_newline = true;
                new_has_final_newline = matches!(line.as_bytes().first(), Some(b'-'));
                index += 1;
            }
        }
        hunks.push(UnifiedHunk {
            old_start,
            old_lines,
            new_lines,
            changes_final_newline,
            new_has_final_newline,
        });
    }
    if hunks.is_empty() {
        bail!("review fix update contains no unified diff hunks");
    }
    Ok(hunks)
}

fn parse_hunk_header(line: &str) -> Result<(usize, usize, usize)> {
    let Some(header) = line.strip_prefix("@@ -") else {
        bail!("review fix update contains content outside a unified diff hunk");
    };
    let (old_range, header) = header
        .split_once(" +")
        .context("review fix update contains an invalid unified diff header")?;
    let (new_range, _) = header
        .split_once(" @@")
        .context("review fix update contains an invalid unified diff header")?;
    let (old_start, old_count) = parse_range(old_range)?;
    let (_, new_count) = parse_range(new_range)?;
    Ok((old_start, old_count, new_count))
}

fn parse_range(range: &str) -> Result<(usize, usize)> {
    let (start, count) = range
        .split_once(',')
        .map_or((range, "1"), |(start, count)| (start, count));
    let start = start
        .parse::<usize>()
        .context("review fix update contains an invalid unified diff range")?;
    let count = count
        .parse::<usize>()
        .context("review fix update contains an invalid unified diff count")?;
    Ok((start, count))
}

fn locate_old_lines(
    lines: &[String],
    old_lines: &[String],
    expected: usize,
    search_start: usize,
) -> Result<usize> {
    if old_lines.is_empty() {
        if expected <= lines.len() {
            return Ok(expected.max(search_start));
        }
        bail!("review fix update insertion is outside the indexed content");
    }
    if matches_at(lines, old_lines, expected) && expected >= search_start {
        return Ok(expected);
    }
    let matches = (search_start..=lines.len().saturating_sub(old_lines.len()))
        .filter(|start| matches_at(lines, old_lines, *start))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [position] => Ok(*position),
        [] => bail!("review fix update does not apply to the indexed content"),
        _ => bail!("review fix update is ambiguous in the indexed content"),
    }
}

fn matches_at(lines: &[String], old_lines: &[String], start: usize) -> bool {
    lines.get(start..start.saturating_add(old_lines.len())) == Some(old_lines)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn applies_a_unified_diff_with_an_offset() {
        assert_eq!(
            apply_update_diff(
                "inserted\none\ntwo\nthree\n",
                "@@ -1,2 +1,2 @@\n one\n-two\n+updated\n"
            )
            .expect("valid diff"),
            Some("inserted\none\nupdated\nthree\n".to_string())
        );
    }

    #[test]
    fn rejects_a_second_file_patch() {
        let error = parse_unified_diff("@@ -1 +1 @@\n-old\n+new\ndiff --git a/outside b/outside\n")
            .expect_err("file header must be rejected");
        assert!(error.to_string().contains("outside a unified diff hunk"));
    }
}
