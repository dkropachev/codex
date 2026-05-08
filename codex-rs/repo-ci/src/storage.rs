use crate::SourceHash;
use crate::SourceKind;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const SOURCE_HASH_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;
const PRUNE_THROTTLE_SECS: u64 = 24 * 60 * 60;
const LAST_HIT_FILENAME: &str = ".last_hit_unix_sec";
const LAST_PRUNE_FILENAME: &str = ".last_prune_unix_sec";

pub(crate) fn artifact_state_dir(
    codex_home: &Path,
    repo_root: &Path,
    sources: &[SourceHash],
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
    repo_artifacts_dir(codex_home, repo_key).join(source_key)
}

pub(crate) fn repo_artifacts_dir(codex_home: &Path, repo_key: &str) -> PathBuf {
    let first = &repo_key[..2];
    let second = &repo_key[2..4];
    codex_home
        .join("repo-ci")
        .join("artifacts")
        .join(first)
        .join(second)
        .join(repo_key)
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

pub(crate) fn source_key(sources: &[SourceHash]) -> String {
    let mut hasher = Sha256::new();
    for source in sources {
        hasher.update(b"path\0");
        hasher.update(source.path.to_string_lossy().as_bytes());
        hasher.update(b"\0kind\0");
        hasher.update(match source.kind {
            SourceKind::CiWorkflow => b"ci_workflow" as &[u8],
            SourceKind::BuildManifest => b"build_manifest" as &[u8],
            SourceKind::Lockfile => b"lockfile" as &[u8],
            SourceKind::Tooling => b"tooling" as &[u8],
        });
        hasher.update(b"\0sha256\0");
        hasher.update(source.sha256.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn record_artifact_hit(state_dir: &Path) {
    if state_dir.is_dir() {
        let now_unix_sec = unix_now();
        let _ = fs::write(last_hit_path(state_dir), format!("{now_unix_sec}\n"));
    }
}

pub(crate) fn prune_stale_artifacts(codex_home: &Path) {
    prune_stale_artifacts_with_now(codex_home, unix_now());
}

pub(crate) fn latest_artifact_state_dir(codex_home: &Path, repo_key: &str) -> Option<PathBuf> {
    let mut candidates = read_dir_paths(&repo_artifacts_dir(codex_home, repo_key))
        .into_iter()
        .filter(|path| path.is_dir() && path.join("manifest.json").is_file())
        .map(|path| (artifact_last_used_unix_sec(&path).unwrap_or(0), path))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    candidates.into_iter().map(|(_, path)| path).next()
}

fn prune_stale_artifacts_with_now(codex_home: &Path, now_unix_sec: u64) {
    let root = codex_home.join("repo-ci").join("artifacts");
    if !root.is_dir() {
        return;
    }
    if let Some(last_prune_unix_sec) = read_unix_sec(&last_prune_path(&root))
        && now_unix_sec.saturating_sub(last_prune_unix_sec) < PRUNE_THROTTLE_SECS
    {
        return;
    }

    let cutoff = now_unix_sec.saturating_sub(SOURCE_HASH_RETENTION_SECS);
    for first in read_dir_paths(&root) {
        for second in read_dir_paths(&first) {
            for repo_dir in read_dir_paths(&second) {
                for state_dir in read_dir_paths(&repo_dir) {
                    if state_dir.is_dir()
                        && let Some(last_used_unix_sec) = artifact_last_used_unix_sec(&state_dir)
                        && last_used_unix_sec < cutoff
                    {
                        let _ = fs::remove_dir_all(state_dir);
                    }
                }
            }
        }
    }
    let _ = fs::write(last_prune_path(&root), format!("{now_unix_sec}\n"));
}

fn read_dir_paths(path: &Path) -> Vec<PathBuf> {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()))
        .collect()
}

fn read_unix_sec(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn artifact_last_used_unix_sec(state_dir: &Path) -> Option<u64> {
    read_unix_sec(&last_hit_path(state_dir)).or_else(|| {
        ["manifest.json", "run_ci.sh"]
            .into_iter()
            .filter_map(|name| {
                fs::metadata(state_dir.join(name))
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs())
            })
            .max()
    })
}

fn last_hit_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LAST_HIT_FILENAME)
}

fn last_prune_path(artifact_root: &Path) -> PathBuf {
    artifact_root.join(LAST_PRUNE_FILENAME)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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
    use std::fs;
    use tempfile::TempDir;

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
        let sources = vec![source_hash("Cargo.toml", "aaa", SourceKind::BuildManifest)];
        let repo_key = repo_key(repo_root);
        let source_key = source_key(&sources);
        let expected = codex_home
            .join("repo-ci")
            .join("artifacts")
            .join(&repo_key[..2])
            .join(&repo_key[2..4])
            .join(&repo_key)
            .join(&source_key);

        assert_eq!(
            artifact_state_dir(codex_home, repo_root, &sources),
            expected
        );
    }

    #[test]
    fn same_remote_and_sources_share_artifact_location() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let origin = temp.path().join("origin.git");
        let source = temp.path().join("source");
        let first = temp.path().join("first");
        let second = temp.path().join("second");

        git(
            temp.path(),
            &["init", "--bare", origin.to_str().expect("utf8 path")],
        );
        fs::create_dir(&source).expect("create source");
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "repo-ci@example.com"]);
        git(&source, &["config", "user.name", "Repo CI"]);
        fs::write(source.join("README.md"), "hello\n").expect("write readme");
        git(&source, &["add", "README.md"]);
        git(&source, &["commit", "-m", "initial"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                origin.to_str().expect("utf8 path"),
            ],
        );
        git(&source, &["push", "origin", "HEAD:main"]);
        git(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            temp.path(),
            &[
                "clone",
                origin.to_str().expect("utf8 path"),
                first.to_str().expect("utf8 path"),
            ],
        );
        git(
            temp.path(),
            &[
                "clone",
                origin.to_str().expect("utf8 path"),
                second.to_str().expect("utf8 path"),
            ],
        );

        let sources = vec![source_hash("Cargo.toml", "aaa", SourceKind::BuildManifest)];
        let first_location = artifact_state_dir(&codex_home, &first, &sources);
        let second_location = artifact_state_dir(&codex_home, &second, &sources);

        assert_eq!(first_location, second_location);
    }

    #[test]
    fn commit_hash_does_not_change_artifact_location_without_source_changes() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "repo-ci@example.com"]);
        git(&repo, &["config", "user.name", "Repo CI"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:openai/codex.git"],
        );
        fs::write(repo.join("README.md"), "one\n").expect("write readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "first"]);
        let sources = vec![source_hash("Cargo.toml", "aaa", SourceKind::BuildManifest)];
        let first_location = artifact_state_dir(&codex_home, &repo, &sources);

        fs::write(repo.join("README.md"), "two\n").expect("update readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "second"]);
        let second_location = artifact_state_dir(&codex_home, &repo, &sources);

        assert_eq!(first_location, second_location);
    }

    #[test]
    fn source_hash_changes_artifact_location() {
        let codex_home = Path::new("/tmp/codex-home");
        let repo_root = Path::new("/tmp/repo");
        let first_sources = vec![source_hash("Cargo.toml", "aaa", SourceKind::BuildManifest)];
        let second_sources = vec![source_hash("Cargo.toml", "bbb", SourceKind::BuildManifest)];

        let first_location = artifact_state_dir(codex_home, repo_root, &first_sources);
        let second_location = artifact_state_dir(codex_home, repo_root, &second_sources);

        assert_ne!(first_location, second_location);
    }

    #[test]
    fn stale_source_hash_artifacts_are_pruned_after_a_week_without_hits() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let old_source_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let fresh_source_key = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let old_dir = artifact_state_dir_for_keys(&codex_home, repo_key, old_source_key);
        let fresh_dir = artifact_state_dir_for_keys(&codex_home, repo_key, fresh_source_key);
        fs::create_dir_all(&old_dir).expect("old artifact dir");
        fs::create_dir_all(&fresh_dir).expect("fresh artifact dir");
        fs::write(last_hit_path(&old_dir), "10\n").expect("old hit");
        fs::write(
            last_hit_path(&fresh_dir),
            format!("{}\n", SOURCE_HASH_RETENTION_SECS + 10),
        )
        .expect("fresh hit");

        prune_stale_artifacts_with_now(&codex_home, SOURCE_HASH_RETENTION_SECS + 11);

        assert!(!old_dir.exists());
        assert!(fresh_dir.exists());
    }

    #[test]
    fn pruning_uses_artifact_mtime_when_last_hit_is_missing() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let source_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let state_dir = artifact_state_dir_for_keys(&codex_home, repo_key, source_key);
        fs::create_dir_all(&state_dir).expect("artifact dir");
        fs::write(state_dir.join("manifest.json"), "{}\n").expect("manifest");
        let last_used = artifact_last_used_unix_sec(&state_dir).expect("mtime fallback");

        prune_stale_artifacts_with_now(&codex_home, last_used + SOURCE_HASH_RETENTION_SECS + 1);

        assert!(!state_dir.exists());
    }

    #[test]
    fn fresh_artifacts_without_last_hit_are_kept_by_mtime() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let source_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let state_dir = artifact_state_dir_for_keys(&codex_home, repo_key, source_key);
        fs::create_dir_all(&state_dir).expect("artifact dir");
        fs::write(state_dir.join("manifest.json"), "{}\n").expect("manifest");
        let last_used = artifact_last_used_unix_sec(&state_dir).expect("mtime fallback");

        prune_stale_artifacts_with_now(&codex_home, last_used + SOURCE_HASH_RETENTION_SECS - 1);

        assert!(state_dir.exists());
    }

    #[test]
    fn cache_pruning_is_throttled_to_once_per_day() {
        let temp = TempDir::new().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let first_source_key = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second_source_key = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let first_dir = artifact_state_dir_for_keys(&codex_home, repo_key, first_source_key);
        fs::create_dir_all(&first_dir).expect("first artifact dir");
        fs::write(last_hit_path(&first_dir), "10\n").expect("first hit");
        let first_prune = SOURCE_HASH_RETENTION_SECS + 11;

        prune_stale_artifacts_with_now(&codex_home, first_prune);

        assert!(!first_dir.exists());

        let second_dir = artifact_state_dir_for_keys(&codex_home, repo_key, second_source_key);
        fs::create_dir_all(&second_dir).expect("second artifact dir");
        fs::write(last_hit_path(&second_dir), "10\n").expect("second hit");

        prune_stale_artifacts_with_now(&codex_home, first_prune + 1);

        assert!(second_dir.exists());

        prune_stale_artifacts_with_now(&codex_home, first_prune + PRUNE_THROTTLE_SECS);

        assert!(!second_dir.exists());
    }

    fn source_hash(path: &str, sha256: &str, kind: SourceKind) -> SourceHash {
        SourceHash {
            path: PathBuf::from(path),
            sha256: sha256.to_string(),
            kind,
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
