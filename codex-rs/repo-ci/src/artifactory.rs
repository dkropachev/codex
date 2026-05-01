use anyhow::Result;
use codex_artifactory::Artifactory;
use codex_artifactory::CacheEntry;
use codex_artifactory::PruneOptions;
use codex_artifactory::StateRegistration;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub(crate) use codex_artifactory::ArtifactSource;

const NAMESPACE: &str = "repo-ci";
const SOURCE_HASH_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;
const PRUNE_THROTTLE_SECS: i64 = 24 * 60 * 60;

#[cfg(test)]
pub(crate) fn artifact_state_dir(
    codex_home: &Path,
    repo_root: &Path,
    sources: &[ArtifactSource],
) -> PathBuf {
    let repo_key = repo_key(repo_root);
    let source_key = source_key(sources);
    artifact_state_dir_for_keys(codex_home, &repo_key, &source_key)
}

pub(crate) fn artifact_state_dir_for_keys(
    codex_home: &Path,
    repo_key: &str,
    source_key: &str,
) -> PathBuf {
    codex_artifactory::sharded_state_dir(&artifact_root(codex_home), repo_key, source_key)
}

#[cfg(test)]
pub(crate) fn repo_artifacts_dir(codex_home: &Path, repo_key: &str) -> PathBuf {
    codex_artifactory::scope_artifacts_dir(&artifact_root(codex_home), repo_key)
}

pub(crate) fn source_key(sources: &[ArtifactSource]) -> String {
    codex_artifactory::source_key(sources)
}

pub(crate) fn changed_source_paths(
    learned_sources: &[ArtifactSource],
    current_sources: &[ArtifactSource],
) -> Vec<PathBuf> {
    codex_artifactory::changed_source_paths(learned_sources, current_sources)
}

pub(crate) fn register_state(
    codex_home: &Path,
    repo_key: &str,
    source_key: &str,
    state_dir: &Path,
    sources: &[ArtifactSource],
    metadata: Value,
) -> Result<()> {
    let mut store = Artifactory::open(codex_home)?;
    store.register_state(&StateRegistration {
        namespace: NAMESPACE.to_string(),
        scope_key: repo_key.to_string(),
        source_key: source_key.to_string(),
        state_dir: state_dir.to_path_buf(),
        sources: sources.to_vec(),
        metadata_json: serde_json::to_string(&metadata)?,
    })?;
    Ok(())
}

pub(crate) fn record_artifact_hit(codex_home: &Path, state_dir: &Path) -> Result<()> {
    let store = Artifactory::open(codex_home)?;
    store.record_state_hit_by_dir(NAMESPACE, state_dir)
}

#[cfg(test)]
pub(crate) fn artifact_last_hit_unix_sec(
    codex_home: &Path,
    state_dir: &Path,
) -> Result<Option<i64>> {
    let store = Artifactory::open(codex_home)?;
    Ok(store
        .state_by_dir(NAMESPACE, state_dir)?
        .and_then(|state| state.last_hit_at_unix_sec))
}

pub(crate) fn prune_stale_artifacts(codex_home: &Path) -> Result<()> {
    let store = Artifactory::open(codex_home)?;
    store.prune_stale_states(
        NAMESPACE,
        PruneOptions::new(SOURCE_HASH_RETENTION_SECS, PRUNE_THROTTLE_SECS),
    )?;
    Ok(())
}

pub(crate) fn latest_artifact_state_dirs(
    codex_home: &Path,
    repo_key: &str,
) -> Result<Vec<PathBuf>> {
    let store = Artifactory::open(codex_home)?;
    Ok(store
        .states_for_scope(NAMESPACE, repo_key)?
        .into_iter()
        .map(|state| state.state_dir)
        .collect())
}

pub(crate) fn index_artifact_file(
    codex_home: &Path,
    state_dir: &Path,
    relative_path: &Path,
) -> Result<()> {
    let store = Artifactory::open(codex_home)?;
    store.index_file(NAMESPACE, state_dir, relative_path)
}

pub(crate) fn artifact_file_path(
    codex_home: &Path,
    relative_path: &Path,
) -> Result<Option<PathBuf>> {
    let store = Artifactory::open(codex_home)?;
    let Some((state, file)) = store.find_file(NAMESPACE, relative_path)? else {
        return Ok(None);
    };
    store.record_state_hit_by_dir(NAMESPACE, &state.state_dir)?;
    Ok(Some(state.state_dir.join(file.relative_path)))
}

pub(crate) fn put_cache_entry(
    codex_home: &Path,
    key: &str,
    artifact_id: &str,
    status: &str,
    metadata: Value,
) -> Result<()> {
    let store = Artifactory::open(codex_home)?;
    store.put_cache_entry(
        NAMESPACE,
        key,
        artifact_id,
        status,
        &serde_json::to_string(&metadata)?,
    )
}

pub(crate) fn cache_entry(codex_home: &Path, key: &str) -> Result<Option<CacheEntry>> {
    let store = Artifactory::open(codex_home)?;
    store.cache_entry(NAMESPACE, key)
}

pub(crate) fn delete_cache_entry(codex_home: &Path, key: &str) -> Result<()> {
    let store = Artifactory::open(codex_home)?;
    store.delete_cache_entry(NAMESPACE, key)
}

pub(crate) fn repo_key(repo_root: &Path) -> String {
    let remote_repo = git_output(repo_root, &["remote", "get-url", "origin"])
        .or_else(|| {
            let remotes = git_output(repo_root, &["remote"])?;
            remotes
                .lines()
                .map(str::trim)
                .find(|remote| !remote.is_empty())
                .and_then(|remote| git_output(repo_root, &["remote", "get-url", remote]))
        })
        .map(|remote| normalize_remote_repo(&remote));
    let identity = remote_repo
        .map(|remote_repo| format!("remote:{remote_repo}"))
        .unwrap_or_else(|| format!("local:{}", repo_root.to_string_lossy()));
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn artifact_root(codex_home: &Path) -> PathBuf {
    codex_home.join("repo-ci").join("artifacts")
}

fn git_output(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn normalize_remote_repo(remote_url: &str) -> String {
    let remote = remote_url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let parsed = (|| -> Option<(String, String)> {
        if let Some(rest) = remote
            .strip_prefix("https://")
            .or_else(|| remote.strip_prefix("http://"))
            .or_else(|| remote.strip_prefix("ssh://"))
            .or_else(|| remote.strip_prefix("git://"))
        {
            let rest = rest.split(['?', '#']).next().unwrap_or(rest);
            let (host, path) = rest.split_once('/')?;
            let host = host.rsplit('@').next()?.to_string();
            return Some((host, trim_remote_path(path)));
        }

        if remote.contains("://") {
            return None;
        }

        let (host, path) = remote.split_once(':')?;
        if host.contains('/') {
            return None;
        }
        let host = host.rsplit('@').next()?.to_string();
        Some((host, trim_remote_path(path)))
    })();
    parsed
        .map(|(host, path)| {
            let host = host.to_ascii_lowercase();
            let path = if host == "github.com" {
                path.to_ascii_lowercase()
            } else {
                path
            };
            format!("{host}/{path}")
        })
        .unwrap_or_else(|| remote.to_string())
}

fn trim_remote_path(path: &str) -> String {
    path.trim_matches('/').trim_end_matches(".git").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalizes_common_remote_url_forms() {
        assert_eq!(
            normalize_remote_repo("git@github.com:OpenAI/codex.git"),
            "github.com/openai/codex"
        );
        assert_eq!(
            normalize_remote_repo("https://github.com/openai/codex.git"),
            "github.com/openai/codex"
        );
        assert_eq!(
            normalize_remote_repo("ssh://git@github.com/openai/codex.git"),
            "github.com/openai/codex"
        );
    }

    #[test]
    fn artifact_location_is_sharded_under_repo_ci_artifacts() {
        let codex_home = Path::new("/tmp/codex-home");
        let repo_root = Path::new("/tmp/repo");
        let sources = vec![ArtifactSource::new(
            PathBuf::from("Cargo.toml"),
            "build_manifest",
            "aaa".to_string(),
        )];
        let repo_key = repo_key(repo_root);
        let source_key = source_key(&sources);

        assert_eq!(
            artifact_state_dir(codex_home, repo_root, &sources),
            codex_home
                .join("repo-ci")
                .join("artifacts")
                .join(&repo_key[..2])
                .join(&repo_key[2..4])
                .join(&repo_key)
                .join(&source_key)
        );
    }
}
