use crate::store;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

const RUN_ARTIFACTS_DIR: &str = "run-artifacts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    Prepare,
    Fast,
    Full,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Fast => "fast",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunArtifact {
    pub artifact_id: String,
    pub repo_root: PathBuf,
    pub repo_key: String,
    pub state_dir: PathBuf,
    pub mode: RunMode,
    pub status: RunArtifactStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub started_at_unix_sec: u64,
    pub manifest_fingerprint: String,
    pub worktree_fingerprint: String,
    pub steps: Vec<StepStatus>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunArtifactStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepStatus {
    pub id: String,
    pub status: StepRunStatus,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRunStatus {
    Started,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCacheContext {
    pub repo_key: String,
    pub manifest_fingerprint: String,
    pub worktree_fingerprint: String,
    pub mode: RunMode,
}

impl RunCacheContext {
    pub fn from_artifact(artifact: &RunArtifact) -> Self {
        Self {
            repo_key: artifact.repo_key.clone(),
            manifest_fingerprint: artifact.manifest_fingerprint.clone(),
            worktree_fingerprint: artifact.worktree_fingerprint.clone(),
            mode: artifact.mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RunCacheEntry {
    artifact_id: String,
    repo_key: String,
    manifest_fingerprint: String,
    worktree_fingerprint: String,
    mode: RunMode,
    cached_at_unix_sec: u64,
}

pub fn worktree_fingerprint_for_repo(repo_root: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for args in [
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"][..],
        &["diff", "--no-ext-diff", "--binary"][..],
        &["diff", "--cached", "--no-ext-diff", "--binary"][..],
    ] {
        hasher.update(b"git\0");
        hasher.update(args.join(" ").as_bytes());
        match Command::new("git")
            .args(args)
            .current_dir(repo_root)
            .output()
        {
            Ok(output) => {
                hasher.update(b"\0status\0");
                hasher.update(output.status.code().unwrap_or(-1).to_string().as_bytes());
                hasher.update(b"\0stdout\0");
                hasher.update(&output.stdout);
                hasher.update(b"\0stderr\0");
                hasher.update(&output.stderr);
            }
            Err(err) => {
                hasher.update(b"\0error\0");
                hasher.update(err.to_string().as_bytes());
            }
        }
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn lookup_cached_passing_run(
    codex_home: &Path,
    context: &RunCacheContext,
) -> Result<Option<RunArtifact>> {
    store::prune_stale_artifacts(codex_home)?;
    let key = cache_key(context);
    let Some(cache) = store::cache_entry(codex_home, &key)? else {
        return Ok(None);
    };
    if cache.status != "passed" {
        store::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    let Ok(entry) = serde_json::from_str::<RunCacheEntry>(&cache.metadata_json) else {
        store::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    };
    if !cache_entry_matches_current(&entry, context) || entry.artifact_id != cache.artifact_id {
        store::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    let Some(artifact) =
        read_run_artifact_if_present(codex_home, &entry.artifact_id).inspect_err(|_| {
            let _ = store::delete_cache_entry(codex_home, &key);
        })?
    else {
        store::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    };
    if !artifact_matches_cache_entry(&artifact, &entry) {
        store::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    Ok(Some(artifact))
}

pub fn write_run_artifact(
    codex_home: &Path,
    state_dir: &Path,
    artifact: &RunArtifact,
) -> Result<()> {
    let path = run_artifact_path(state_dir, &artifact.artifact_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let data = serde_json::to_vec_pretty(artifact)?;
    fs::write(&path, data).with_context(|| format!("failed to write {}", path.display()))?;
    store::index_artifact_file(
        codex_home,
        state_dir,
        &run_artifact_relative_path(&artifact.artifact_id),
    )
}

pub fn put_passing_run_cache(codex_home: &Path, artifact: &RunArtifact) -> Result<()> {
    if artifact.status != RunArtifactStatus::Passed {
        anyhow::bail!("only passed CI/CD run artifacts can be cached");
    }
    let context = RunCacheContext::from_artifact(artifact);
    let entry = RunCacheEntry {
        artifact_id: artifact.artifact_id.clone(),
        repo_key: artifact.repo_key.clone(),
        manifest_fingerprint: artifact.manifest_fingerprint.clone(),
        worktree_fingerprint: artifact.worktree_fingerprint.clone(),
        mode: artifact.mode,
        cached_at_unix_sec: unix_now(),
    };
    store::put_cache_entry(
        codex_home,
        &cache_key(&context),
        &artifact.artifact_id,
        "passed",
        json!(entry),
    )
}

pub fn read_run_artifact(codex_home: &Path, artifact_id: &str) -> Result<RunArtifact> {
    read_run_artifact_if_present(codex_home, artifact_id)?
        .with_context(|| format!("CI/CD run artifact `{artifact_id}` was not found"))
}

fn read_run_artifact_if_present(
    codex_home: &Path,
    artifact_id: &str,
) -> Result<Option<RunArtifact>> {
    let Some(path) = find_run_artifact(codex_home, artifact_id)? else {
        return Ok(None);
    };
    read_run_artifact_at_if_present(&path)
}

fn read_run_artifact_at_if_present(path: &Path) -> Result<Option<RunArtifact>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map(Some)
}

pub fn run_artifact_path(state_dir: &Path, artifact_id: &str) -> PathBuf {
    state_dir.join(run_artifact_relative_path(artifact_id))
}

pub fn run_artifact_relative_path(artifact_id: &str) -> PathBuf {
    PathBuf::from(RUN_ARTIFACTS_DIR).join(format!("{artifact_id}.json"))
}

pub fn cache_key(context: &RunCacheContext) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"repo\0");
    hasher.update(context.repo_key.as_bytes());
    hasher.update(b"\0manifest\0");
    hasher.update(context.manifest_fingerprint.as_bytes());
    hasher.update(b"\0worktree\0");
    hasher.update(context.worktree_fingerprint.as_bytes());
    hasher.update(b"\0mode\0");
    hasher.update(context.mode.as_str().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn artifact_id(
    repo_key: &str,
    manifest_fingerprint: &str,
    worktree_fingerprint: &str,
    mode: RunMode,
    started_at_unix_nanos: u128,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_key.as_bytes());
    hasher.update(manifest_fingerprint.as_bytes());
    hasher.update(worktree_fingerprint.as_bytes());
    hasher.update(mode.as_str().as_bytes());
    hasher.update(started_at_unix_nanos.to_string().as_bytes());
    hasher.update(std::process::id().to_string().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_entry_matches_current(entry: &RunCacheEntry, context: &RunCacheContext) -> bool {
    entry.repo_key == context.repo_key
        && entry.manifest_fingerprint == context.manifest_fingerprint
        && entry.worktree_fingerprint == context.worktree_fingerprint
        && entry.mode == context.mode
}

fn artifact_matches_cache_entry(artifact: &RunArtifact, entry: &RunCacheEntry) -> bool {
    artifact.artifact_id == entry.artifact_id
        && artifact.repo_key == entry.repo_key
        && artifact.manifest_fingerprint == entry.manifest_fingerprint
        && artifact.worktree_fingerprint == entry.worktree_fingerprint
        && artifact.mode == entry.mode
        && artifact.status == RunArtifactStatus::Passed
}

fn find_run_artifact(codex_home: &Path, artifact_id: &str) -> Result<Option<PathBuf>> {
    if !artifact_id
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(None);
    }
    store::artifact_file_path(codex_home, &run_artifact_relative_path(artifact_id))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactSource;
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn worktree_fingerprint_changes_with_diff() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "cicd@example.com"]);
        git(&repo, &["config", "user.name", "CI/CD"]);
        fs::write(repo.join("file.txt"), "one\n").expect("write file");
        git(&repo, &["add", "file.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        let first = worktree_fingerprint_for_repo(&repo).expect("first fingerprint");

        fs::write(repo.join("file.txt"), "two\n").expect("update file");

        assert_ne!(
            first,
            worktree_fingerprint_for_repo(&repo).expect("second fingerprint")
        );
    }

    #[test]
    fn artifact_write_read_and_cached_pass_lookup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let state_dir = temp.path().join("state");
        let context = context();
        register_state(&codex_home, &state_dir, &context);
        let artifact = artifact_for(hex_id('a'), &state_dir, &context, RunArtifactStatus::Passed);

        write_run_artifact(&codex_home, &state_dir, &artifact).expect("write artifact");
        put_passing_run_cache(&codex_home, &artifact).expect("cache artifact");

        assert_eq!(
            read_run_artifact(&codex_home, &artifact.artifact_id).expect("read"),
            artifact
        );
        assert_eq!(
            lookup_cached_passing_run(&codex_home, &context).expect("lookup"),
            Some(artifact)
        );
    }

    #[test]
    fn missing_sqlite_cached_artifact_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let context = context();
        let artifact_id = hex_id('d');
        store::put_cache_entry(
            &codex_home,
            &cache_key(&context),
            &artifact_id,
            "passed",
            json!(RunCacheEntry {
                artifact_id: artifact_id.clone(),
                repo_key: context.repo_key.clone(),
                manifest_fingerprint: context.manifest_fingerprint.clone(),
                worktree_fingerprint: context.worktree_fingerprint.clone(),
                mode: context.mode,
                cached_at_unix_sec: 1,
            }),
        )
        .expect("put cache");

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &context).expect("lookup"),
            None
        );
        assert!(
            store::cache_entry(&codex_home, &cache_key(&context))
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn corrupt_sqlite_cache_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let context = context();
        store::put_cache_entry(
            &codex_home,
            &cache_key(&context),
            &hex_id('e'),
            "passed",
            json!("not json"),
        )
        .expect("put cache");

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &context).expect("lookup"),
            None
        );
        assert!(
            store::cache_entry(&codex_home, &cache_key(&context))
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn non_passing_cached_artifact_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let state_dir = temp.path().join("state");
        let context = context();
        register_state(&codex_home, &state_dir, &context);
        let artifact = artifact_for(hex_id('f'), &state_dir, &context, RunArtifactStatus::Failed);
        write_run_artifact(&codex_home, &state_dir, &artifact).expect("write artifact");
        write_sqlite_cache_entry(
            &codex_home,
            &context,
            &artifact,
            /*cached_at_unix_sec*/ 1,
        );

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &context).expect("lookup"),
            None
        );
        assert!(
            store::cache_entry(&codex_home, &cache_key(&context))
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn corrupt_cached_artifact_errors_and_evicts_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let state_dir = temp.path().join("state");
        let context = context();
        register_state(&codex_home, &state_dir, &context);
        let artifact = artifact_for(hex_id('1'), &state_dir, &context, RunArtifactStatus::Passed);
        let path = run_artifact_path(&state_dir, &artifact.artifact_id);
        fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
        fs::write(&path, b"not json").expect("write corrupt artifact");
        store::index_artifact_file(
            &codex_home,
            &state_dir,
            &run_artifact_relative_path(&artifact.artifact_id),
        )
        .expect("index artifact");
        write_sqlite_cache_entry(
            &codex_home,
            &context,
            &artifact,
            /*cached_at_unix_sec*/ 1,
        );

        let err = lookup_cached_passing_run(&codex_home, &context)
            .expect_err("corrupt artifact should error");

        assert!(err.to_string().contains("failed to parse"));
        assert!(
            store::cache_entry(&codex_home, &cache_key(&context))
                .expect("cache entry")
                .is_none()
        );
    }

    fn context() -> RunCacheContext {
        RunCacheContext {
            repo_key: "repo".to_string(),
            manifest_fingerprint: "manifest".to_string(),
            worktree_fingerprint: "worktree".to_string(),
            mode: RunMode::Fast,
        }
    }

    fn register_state(codex_home: &Path, state_dir: &Path, context: &RunCacheContext) {
        fs::create_dir_all(state_dir).expect("state dir");
        store::register_state(
            codex_home,
            &context.repo_key,
            "source",
            state_dir,
            &[ArtifactSource::new(
                PathBuf::from("Cargo.toml"),
                "build_manifest",
                "abc".to_string(),
            )],
            json!({}),
        )
        .expect("register state");
    }

    fn artifact_for(
        artifact_id: String,
        state_dir: &Path,
        context: &RunCacheContext,
        status: RunArtifactStatus,
    ) -> RunArtifact {
        RunArtifact {
            artifact_id,
            repo_root: PathBuf::from("/repo"),
            repo_key: context.repo_key.clone(),
            state_dir: state_dir.to_path_buf(),
            mode: context.mode,
            status,
            exit_code: if status == RunArtifactStatus::Passed {
                Some(0)
            } else {
                Some(1)
            },
            duration_ms: 1,
            started_at_unix_sec: 1,
            manifest_fingerprint: context.manifest_fingerprint.clone(),
            worktree_fingerprint: context.worktree_fingerprint.clone(),
            steps: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn write_sqlite_cache_entry(
        codex_home: &Path,
        context: &RunCacheContext,
        artifact: &RunArtifact,
        cached_at_unix_sec: u64,
    ) {
        store::put_cache_entry(
            codex_home,
            &cache_key(context),
            &artifact.artifact_id,
            "passed",
            json!(RunCacheEntry {
                artifact_id: artifact.artifact_id.clone(),
                repo_key: artifact.repo_key.clone(),
                manifest_fingerprint: artifact.manifest_fingerprint.clone(),
                worktree_fingerprint: artifact.worktree_fingerprint.clone(),
                mode: artifact.mode,
                cached_at_unix_sec,
            }),
        )
        .expect("put cache")
    }

    fn hex_id(ch: char) -> String {
        std::iter::repeat_n(ch, 64).collect()
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
