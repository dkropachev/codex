use anyhow::Context;
use codex_config::CONFIG_TOML_FILE;
use codex_core::config::edit::ConfigEditsBuilder;
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug)]
pub(crate) struct ResolvedRepoCiLearningInstructionScope {
    pub(crate) label: String,
    pub(crate) segments: Vec<String>,
}

pub(crate) fn configured_repo_ci_learning_instruction(
    codex_home: &Path,
    segments: &[String],
) -> anyhow::Result<Option<String>> {
    let singular_segments = append_segment(segments, "learning_instruction");
    if let Some(item) = repo_ci_config_item(codex_home, &singular_segments)?
        && let Some(instruction) = repo_ci_learning_instruction_from_item(&item)
    {
        return Ok(Some(instruction));
    }

    let legacy_segments = append_segment(segments, "learning_instructions");
    let Some(item) = repo_ci_config_item(codex_home, &legacy_segments)? else {
        return Ok(None);
    };
    Ok(repo_ci_learning_instruction_from_item(&item))
}

pub(crate) async fn persist_repo_ci_learning_instruction(
    codex_home: &Path,
    segments: &[String],
    instruction: Option<&str>,
) -> anyhow::Result<()> {
    let instruction = instruction.and_then(normalize_repo_ci_learning_instruction);
    let builder = ConfigEditsBuilder::new(codex_home)
        .clear_path(append_segment(segments, "learning_instructions"));
    let builder = if let Some(instruction) = instruction {
        builder.set_path_value(
            append_segment(segments, "learning_instruction"),
            toml_edit::value(instruction),
        )
    } else {
        builder.clear_path(append_segment(segments, "learning_instruction"))
    };
    builder.apply().await
}

pub(crate) fn normalize_repo_ci_learning_instruction(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

pub(crate) fn git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

pub(crate) fn github_repo_slug_for_root(repo_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8(output.stdout).ok()?;
    parse_github_remote_url(remote.trim())
}

pub(crate) fn validate_repo_ci_github_repo(repo: &str) -> Result<(), String> {
    let mut parts = repo.split('/');
    let valid = parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_none();
    if valid {
        Ok(())
    } else {
        Err("repoCiLearningInstruction githubRepo scope must be `org/repo`".to_string())
    }
}

fn repo_ci_config_item(
    codex_home: &Path,
    segments: &[String],
) -> anyhow::Result<Option<toml_edit::Item>> {
    let config_path = codex_home.join(CONFIG_TOML_FILE);
    if !config_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let doc = raw
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let mut item = doc.as_item();
    for segment in segments {
        let Some(next) = item.get(segment) else {
            return Ok(None);
        };
        item = next;
    }
    Ok(Some(item.clone()))
}

fn repo_ci_learning_instruction_from_item(item: &toml_edit::Item) -> Option<String> {
    if let Some(value) = item.as_str() {
        return normalize_repo_ci_learning_instruction(value);
    }
    item.as_array().and_then(|array| {
        normalize_repo_ci_learning_instruction(
            &array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        )
    })
}

fn parse_github_remote_url(remote: &str) -> Option<String> {
    let slug = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("http://github.com/"))
        .or_else(|| remote.strip_prefix("git@github.com:"))?;
    let slug = slug.strip_suffix(".git").unwrap_or(slug).trim_matches('/');
    validate_repo_ci_github_repo(slug).ok()?;
    Some(slug.to_string())
}

fn append_segment(segments: &[String], segment: &str) -> Vec<String> {
    let mut segments = segments.to_vec();
    segments.push(segment.to_string());
    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_github_remote_url_accepts_common_forms() {
        assert_eq!(
            parse_github_remote_url("https://github.com/openai/codex.git"),
            Some("openai/codex".to_string())
        );
        assert_eq!(
            parse_github_remote_url("git@github.com:openai/codex.git"),
            Some("openai/codex".to_string())
        );
    }

    #[test]
    fn parse_github_remote_url_rejects_invalid_scope() {
        assert_eq!(
            parse_github_remote_url("https://example.com/openai/codex"),
            None
        );
        assert_eq!(parse_github_remote_url("https://github.com/openai"), None);
    }
}
