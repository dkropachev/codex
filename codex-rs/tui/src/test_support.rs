pub(crate) use codex_utils_absolute_path::test_support::PathBufExt;
pub(crate) use codex_utils_absolute_path::test_support::test_path_buf;

pub(crate) fn test_path_display(path: &str) -> String {
    test_path_buf(path).display().to_string()
}

pub(crate) fn normalize_codex_version_for_snapshot(
    lines: Vec<String>,
    snapshot_version: &str,
) -> Vec<String> {
    let version_marker = "OpenAI Codex (";

    lines
        .into_iter()
        .map(|line| {
            let Some(version_pos) = line.find(version_marker) else {
                return line;
            };
            let version_start = version_pos + version_marker.len();
            let Some(version_end_offset) = line[version_start..].find(')') else {
                return line;
            };
            let version_end = version_start + version_end_offset;
            let version_len = version_end - version_start;
            let snapshot_version_len = snapshot_version.len();
            let mut suffix = line[version_end..].to_string();

            if snapshot_version_len > version_len {
                let spaces_to_remove = snapshot_version_len - version_len;
                let removable_spaces = suffix[1..]
                    .bytes()
                    .take_while(|byte| *byte == b' ')
                    .take(spaces_to_remove)
                    .count();
                suffix.replace_range(1..1 + removable_spaces, "");
            } else if snapshot_version_len < version_len {
                suffix.insert_str(1, &" ".repeat(version_len - snapshot_version_len));
            }

            let mut rebuilt = line[..version_start].to_string();
            rebuilt.push_str(snapshot_version);
            rebuilt.push_str(&suffix);
            rebuilt
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::normalize_codex_version_for_snapshot;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalize_codex_version_for_snapshot_preserves_line_width() {
        let bazel_line = "│  >_ OpenAI Codex (v0.0.0)     │".to_string();
        let expected = "│  >_ OpenAI Codex (v0.125.0)   │".to_string();

        assert_eq!(
            normalize_codex_version_for_snapshot(vec![bazel_line], "v0.125.0"),
            vec![expected]
        );
    }
}
