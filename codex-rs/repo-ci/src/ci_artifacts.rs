use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::CapturedRun;
use crate::CapturedStep;
use crate::CapturedStepEvent;
use crate::RepoCiManifest;
use crate::RepoCiPaths;
use crate::RunMode;
use crate::SourceHash;
use crate::SourceKind;
use crate::StepPhase;
use crate::ValidationStatus;
use crate::artifactory;
use crate::read_manifest;
use crate::repo_root_for_cwd;
use crate::require_runner;
use crate::runner::RepoCiCancellation;
use crate::runner::capture_runner;

const RUN_ARTIFACTS_DIR: &str = "run-artifacts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCiRunArtifact {
    pub artifact_id: String,
    pub repo_root: PathBuf,
    pub repo_key: String,
    pub state_dir: PathBuf,
    pub mode: RunMode,
    pub status: RepoCiRunArtifactStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub started_at_unix_sec: u64,
    pub manifest_fingerprint: String,
    pub worktree_fingerprint: String,
    pub steps: Vec<RepoCiStepStatus>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoCiRunArtifactStatus {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCiStepStatus {
    pub id: String,
    pub status: RepoCiStepRunStatus,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoCiStepRunStatus {
    Started,
    Passed,
    Failed,
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

pub fn manifest_fingerprint(manifest: &RepoCiManifest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"version\0");
    hasher.update(manifest.version.to_string().as_bytes());
    hasher.update(b"\nautomation\0");
    hasher.update(manifest.automation.as_str().as_bytes());
    hasher.update(b"\nbudget\0");
    hasher.update(manifest.local_test_time_budget_sec.to_string().as_bytes());
    hash_sources(&mut hasher, &manifest.learning_sources);
    hash_steps(&mut hasher, "prepare", &manifest.prepare_steps);
    hash_steps(&mut hasher, "fast", &manifest.fast_steps);
    hash_steps(&mut hasher, "full", &manifest.full_steps);
    hash_validation(&mut hasher, &manifest.validation);
    format!("{:x}", hasher.finalize())
}

pub fn worktree_fingerprint(cwd: &Path) -> Result<String> {
    let repo_root = repo_root_for_cwd(cwd)?;
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
            .current_dir(&repo_root)
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
    cwd: &Path,
    mode: RunMode,
) -> Result<Option<RepoCiRunArtifact>> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let paths = crate::paths_for_repo(codex_home, cwd)?;
    if !paths.manifest_path.exists() {
        return Ok(None);
    }
    let manifest = read_manifest(&paths.manifest_path)?;
    let manifest_fingerprint = manifest_fingerprint(&manifest);
    let worktree_fingerprint = worktree_fingerprint(cwd)?;
    let key = cache_key(
        &manifest.repo_key,
        &manifest_fingerprint,
        &worktree_fingerprint,
        mode,
    );
    let Some(cache) = artifactory::cache_entry(codex_home, &key)? else {
        return Ok(None);
    };
    if cache.status != "passed" {
        artifactory::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    let Ok(entry) = serde_json::from_str::<RunCacheEntry>(&cache.metadata_json) else {
        artifactory::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    };
    if !cache_entry_matches_current(
        &entry,
        &manifest,
        &manifest_fingerprint,
        &worktree_fingerprint,
        mode,
    ) || entry.artifact_id != cache.artifact_id
    {
        artifactory::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    let Some(artifact) =
        read_run_artifact_if_present(codex_home, &entry.artifact_id).inspect_err(|_| {
            let _ = artifactory::delete_cache_entry(codex_home, &key);
        })?
    else {
        artifactory::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    };
    if !artifact_matches_cache_entry(&artifact, &entry) {
        artifactory::delete_cache_entry(codex_home, &key)?;
        return Ok(None);
    }
    Ok(Some(artifact))
}

pub fn run_capture_persisted_with_cancellation(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
    cancellation: RepoCiCancellation,
) -> Result<RepoCiRunArtifact> {
    artifactory::prune_stale_artifacts(codex_home)?;
    let paths = crate::paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    crate::touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
    let started = Instant::now();
    let run = capture_runner(
        &paths,
        mode.as_str(),
        manifest.local_test_time_budget_sec,
        &cancellation,
    )?;
    store_captured_run_artifact(codex_home, &paths, &manifest, mode, &run, started.elapsed())
}

pub fn store_captured_run_artifact(
    codex_home: &Path,
    paths: &RepoCiPaths,
    manifest: &RepoCiManifest,
    mode: RunMode,
    run: &CapturedRun,
    duration: Duration,
) -> Result<RepoCiRunArtifact> {
    crate::register_manifest_artifact_state(codex_home, paths, manifest)?;
    let manifest_fingerprint = manifest_fingerprint(manifest);
    let worktree_fingerprint = worktree_fingerprint(&paths.repo_root)?;
    let started_at = unix_now_duration();
    let started_at_unix_sec = started_at.as_secs();
    let artifact_id = artifact_id(
        &manifest.repo_key,
        &manifest_fingerprint,
        &worktree_fingerprint,
        mode,
        started_at.as_nanos(),
    );
    let artifact = RepoCiRunArtifact {
        artifact_id,
        repo_root: paths.repo_root.clone(),
        repo_key: manifest.repo_key.clone(),
        state_dir: paths.state_dir.clone(),
        mode,
        status: if run.status.success {
            RepoCiRunArtifactStatus::Passed
        } else {
            RepoCiRunArtifactStatus::Failed
        },
        exit_code: run.status.code,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        started_at_unix_sec,
        manifest_fingerprint,
        worktree_fingerprint,
        steps: summarize_steps(&run.steps),
        stdout: run.stdout.clone(),
        stderr: run.stderr.clone(),
    };
    write_run_artifact(codex_home, paths, &artifact)?;
    if artifact.status == RepoCiRunArtifactStatus::Passed {
        write_cache_entry(codex_home, &artifact)?;
    }
    artifactory::record_artifact_hit(codex_home, &paths.state_dir)?;
    Ok(artifact)
}

pub fn read_run_artifact(codex_home: &Path, artifact_id: &str) -> Result<RepoCiRunArtifact> {
    read_run_artifact_if_present(codex_home, artifact_id)?
        .with_context(|| format!("repo-ci run artifact `{artifact_id}` was not found"))
}

fn read_run_artifact_if_present(
    codex_home: &Path,
    artifact_id: &str,
) -> Result<Option<RepoCiRunArtifact>> {
    let Some(path) = find_run_artifact(codex_home, artifact_id)? else {
        return Ok(None);
    };
    read_run_artifact_at_if_present(&path)
}

fn read_run_artifact_at_if_present(path: &Path) -> Result<Option<RepoCiRunArtifact>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    };
    serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse {}", path.display()))
        .map(Some)
}

fn hash_sources(hasher: &mut Sha256, sources: &[SourceHash]) {
    hasher.update(b"\nsources\0");
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
}

fn hash_steps(hasher: &mut Sha256, label: &str, steps: &[crate::RepoCiStep]) {
    hasher.update(b"steps\0");
    hasher.update(label.as_bytes());
    hasher.update(b"\0");
    for step in steps {
        hasher.update(step.id.as_bytes());
        hasher.update(b"\0");
        hasher.update(step.command.as_bytes());
        hasher.update(b"\0");
        hasher.update(match step.phase {
            StepPhase::Prepare => b"prepare" as &[u8],
            StepPhase::Lint => b"lint" as &[u8],
            StepPhase::Build => b"build" as &[u8],
            StepPhase::Test => b"test" as &[u8],
        });
        hasher.update(b"\n");
    }
}

fn hash_validation(hasher: &mut Sha256, validation: &ValidationStatus) {
    hasher.update(b"validation\0");
    match validation {
        ValidationStatus::NotRun => hasher.update(b"not_run"),
        ValidationStatus::Passed {
            validated_at_unix_sec,
        } => {
            hasher.update(b"passed\0");
            hasher.update(validated_at_unix_sec.to_string().as_bytes());
        }
        ValidationStatus::Failed { exit_code } => {
            hasher.update(b"failed\0");
            let exit_code = exit_code.map_or_else(|| "none".to_string(), |code| code.to_string());
            hasher.update(exit_code.as_bytes());
        }
    }
}

fn summarize_steps(steps: &[CapturedStep]) -> Vec<RepoCiStepStatus> {
    let mut by_id = BTreeMap::<String, RepoCiStepStatus>::new();
    for step in steps {
        let status = match step.event {
            CapturedStepEvent::Started => RepoCiStepRunStatus::Started,
            CapturedStepEvent::Finished => {
                if step.exit_code == Some(0) {
                    RepoCiStepRunStatus::Passed
                } else {
                    RepoCiStepRunStatus::Failed
                }
            }
        };
        by_id.insert(
            step.id.clone(),
            RepoCiStepStatus {
                id: step.id.clone(),
                status,
                exit_code: step.exit_code,
            },
        );
    }
    by_id.into_values().collect()
}

fn write_run_artifact(
    codex_home: &Path,
    paths: &RepoCiPaths,
    artifact: &RepoCiRunArtifact,
) -> Result<()> {
    let path = run_artifact_path(paths, &artifact.artifact_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let data = serde_json::to_vec_pretty(artifact)?;
    fs::write(&path, data).with_context(|| format!("failed to write {}", path.display()))?;
    artifactory::index_artifact_file(
        codex_home,
        &paths.state_dir,
        &run_artifact_relative_path(&artifact.artifact_id),
    )
}

fn write_cache_entry(codex_home: &Path, artifact: &RepoCiRunArtifact) -> Result<()> {
    let key = cache_key(
        &artifact.repo_key,
        &artifact.manifest_fingerprint,
        &artifact.worktree_fingerprint,
        artifact.mode,
    );
    let entry = RunCacheEntry {
        artifact_id: artifact.artifact_id.clone(),
        repo_key: artifact.repo_key.clone(),
        manifest_fingerprint: artifact.manifest_fingerprint.clone(),
        worktree_fingerprint: artifact.worktree_fingerprint.clone(),
        mode: artifact.mode,
        cached_at_unix_sec: unix_now(),
    };
    artifactory::put_cache_entry(
        codex_home,
        &key,
        &artifact.artifact_id,
        "passed",
        json!(entry),
    )
}

fn cache_entry_matches_current(
    entry: &RunCacheEntry,
    manifest: &RepoCiManifest,
    manifest_fingerprint: &str,
    worktree_fingerprint: &str,
    mode: RunMode,
) -> bool {
    entry.repo_key == manifest.repo_key
        && entry.manifest_fingerprint == manifest_fingerprint
        && entry.worktree_fingerprint == worktree_fingerprint
        && entry.mode == mode
}

fn artifact_matches_cache_entry(artifact: &RepoCiRunArtifact, entry: &RunCacheEntry) -> bool {
    artifact.artifact_id == entry.artifact_id
        && artifact.repo_key == entry.repo_key
        && artifact.manifest_fingerprint == entry.manifest_fingerprint
        && artifact.worktree_fingerprint == entry.worktree_fingerprint
        && artifact.mode == entry.mode
        && artifact.status == RepoCiRunArtifactStatus::Passed
}

fn run_artifact_path(paths: &RepoCiPaths, artifact_id: &str) -> PathBuf {
    paths
        .state_dir
        .join(run_artifact_relative_path(artifact_id))
}

fn run_artifact_relative_path(artifact_id: &str) -> PathBuf {
    PathBuf::from(RUN_ARTIFACTS_DIR).join(format!("{artifact_id}.json"))
}

fn cache_key(
    repo_key: &str,
    manifest_fingerprint: &str,
    worktree_fingerprint: &str,
    mode: RunMode,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"repo\0");
    hasher.update(repo_key.as_bytes());
    hasher.update(b"\0manifest\0");
    hasher.update(manifest_fingerprint.as_bytes());
    hasher.update(b"\0worktree\0");
    hasher.update(worktree_fingerprint.as_bytes());
    hasher.update(b"\0mode\0");
    hasher.update(mode.as_str().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn artifact_id(
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

fn find_run_artifact(codex_home: &Path, artifact_id: &str) -> Result<Option<PathBuf>> {
    if !artifact_id
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(None);
    }
    artifactory::artifact_file_path(codex_home, &run_artifact_relative_path(artifact_id))
}

fn unix_now() -> u64 {
    unix_now_duration().as_secs()
}

fn unix_now_duration() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AutomationMode;
    use crate::CapturedExitStatus;
    use crate::LearnOptions;
    use crate::LearnedPlan;
    use crate::RepoCiStep;
    use crate::StepPhase;
    use crate::learn_with_plan;
    use pretty_assertions::assert_eq;
    use std::fs;

    #[test]
    fn manifest_fingerprint_changes_when_steps_change() {
        let mut manifest = test_manifest();
        let first = manifest_fingerprint(&manifest);
        manifest.fast_steps.push(RepoCiStep {
            id: "test".to_string(),
            command: "cargo test".to_string(),
            phase: StepPhase::Test,
        });

        assert_ne!(first, manifest_fingerprint(&manifest));
    }

    #[test]
    fn worktree_fingerprint_changes_with_diff() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        git(&repo, &["init"]);
        git(&repo, &["config", "user.email", "repo-ci@example.com"]);
        git(&repo, &["config", "user.name", "Repo CI"]);
        fs::write(repo.join("file.txt"), "one\n").expect("write file");
        git(&repo, &["add", "file.txt"]);
        git(&repo, &["commit", "-m", "initial"]);
        let first = worktree_fingerprint(&repo).expect("first fingerprint");

        fs::write(repo.join("file.txt"), "two\n").expect("update file");

        assert_ne!(
            first,
            worktree_fingerprint(&repo).expect("second fingerprint")
        );
    }

    #[test]
    fn artifact_write_read_and_cached_pass_lookup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        let outcome = learn_with_plan(
            &codex_home,
            &repo,
            LearnOptions {
                automation: AutomationMode::Local,
                local_test_time_budget_sec: 300,
            },
            LearnedPlan {
                prepare_steps: Vec::new(),
                fast_steps: vec![crate::step("ok", "true", StepPhase::Test)],
                full_steps: Vec::new(),
            },
        )
        .expect("learn");
        let run = CapturedRun {
            status: CapturedExitStatus {
                code: Some(0),
                success: true,
            },
            stdout: "ok\n".to_string(),
            stderr: String::new(),
            steps: vec![CapturedStep {
                id: "ok".to_string(),
                event: CapturedStepEvent::Finished,
                exit_code: Some(0),
            }],
        };

        let artifact = store_captured_run_artifact(
            &codex_home,
            &outcome.paths,
            &outcome.manifest,
            RunMode::Fast,
            &run,
            Duration::from_millis(/*millis*/ 12),
        )
        .expect("store artifact");

        assert_eq!(
            read_run_artifact(&codex_home, &artifact.artifact_id).expect("read"),
            artifact
        );
        assert_eq!(
            lookup_cached_passing_run(&codex_home, &repo, RunMode::Fast).expect("cache"),
            Some(artifact)
        );
    }

    #[test]
    fn missing_sqlite_cached_artifact_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        let outcome = learn_fast(&codex_home, &repo);
        let (key, manifest_fingerprint, worktree_fingerprint) =
            current_cache_context(&repo, &outcome.manifest, RunMode::Fast);
        let artifact_id = hex_id('d');
        artifactory::put_cache_entry(
            &codex_home,
            &key,
            &artifact_id,
            "passed",
            json!(RunCacheEntry {
                artifact_id: artifact_id.clone(),
                repo_key: outcome.manifest.repo_key,
                manifest_fingerprint,
                worktree_fingerprint,
                mode: RunMode::Fast,
                cached_at_unix_sec: 1,
            }),
        )
        .expect("put cache");

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &repo, RunMode::Fast).expect("lookup"),
            None
        );
        assert!(
            artifactory::cache_entry(&codex_home, &key)
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn corrupt_sqlite_cache_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        let outcome = learn_fast(&codex_home, &repo);
        let (key, _manifest_fingerprint, _worktree_fingerprint) =
            current_cache_context(&repo, &outcome.manifest, RunMode::Fast);
        codex_artifactory::Artifactory::open(&codex_home)
            .expect("open store")
            .put_cache_entry("repo-ci", &key, &hex_id('e'), "passed", "not json")
            .expect("put cache");

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &repo, RunMode::Fast).expect("lookup"),
            None
        );
        assert!(
            artifactory::cache_entry(&codex_home, &key)
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn non_passing_cached_artifact_is_evicted_and_misses() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        let outcome = learn_fast(&codex_home, &repo);
        let (key, manifest_fingerprint, worktree_fingerprint) =
            current_cache_context(&repo, &outcome.manifest, RunMode::Fast);
        let artifact = artifact_for(
            hex_id('f'),
            &outcome.paths,
            &outcome.manifest,
            RunMode::Fast,
            RepoCiRunArtifactStatus::Failed,
            &manifest_fingerprint,
            &worktree_fingerprint,
        );
        write_artifact_file(&outcome.paths, &artifact);
        artifactory::index_artifact_file(
            &codex_home,
            &outcome.paths.state_dir,
            &run_artifact_relative_path(&artifact.artifact_id),
        )
        .expect("index artifact");
        write_sqlite_cache_entry(&codex_home, &key, &artifact, 1);

        assert_eq!(
            lookup_cached_passing_run(&codex_home, &repo, RunMode::Fast).expect("lookup"),
            None
        );
        assert!(
            artifactory::cache_entry(&codex_home, &key)
                .expect("cache entry")
                .is_none()
        );
    }

    #[test]
    fn corrupt_cached_artifact_errors_and_evicts_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let codex_home = temp.path().join("codex-home");
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).expect("create repo");
        let outcome = learn_fast(&codex_home, &repo);
        let (key, manifest_fingerprint, worktree_fingerprint) =
            current_cache_context(&repo, &outcome.manifest, RunMode::Fast);
        let artifact = artifact_for(
            hex_id('1'),
            &outcome.paths,
            &outcome.manifest,
            RunMode::Fast,
            RepoCiRunArtifactStatus::Passed,
            &manifest_fingerprint,
            &worktree_fingerprint,
        );
        let path = run_artifact_path(&outcome.paths, &artifact.artifact_id);
        fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
        fs::write(&path, b"not json").expect("write corrupt artifact");
        artifactory::index_artifact_file(
            &codex_home,
            &outcome.paths.state_dir,
            &run_artifact_relative_path(&artifact.artifact_id),
        )
        .expect("index artifact");
        write_sqlite_cache_entry(&codex_home, &key, &artifact, 1);

        let err = lookup_cached_passing_run(&codex_home, &repo, RunMode::Fast)
            .expect_err("corrupt artifact should error");

        assert!(err.to_string().contains("failed to parse"));
        assert!(
            artifactory::cache_entry(&codex_home, &key)
                .expect("cache entry")
                .is_none()
        );
    }

    fn test_manifest() -> RepoCiManifest {
        RepoCiManifest {
            version: 3,
            repo_root: PathBuf::from("/tmp/repo"),
            repo_key: "repo".to_string(),
            source_key: "source".to_string(),
            automation: AutomationMode::Local,
            local_test_time_budget_sec: 300,
            learned_at_unix_sec: 1,
            learning_sources: vec![SourceHash {
                path: PathBuf::from("Cargo.toml"),
                sha256: "abc".to_string(),
                kind: SourceKind::BuildManifest,
            }],
            inferred_issue_types: Vec::new(),
            prepare_steps: Vec::new(),
            fast_steps: Vec::new(),
            full_steps: Vec::new(),
            validation: ValidationStatus::NotRun,
        }
    }

    fn learn_fast(codex_home: &Path, repo: &Path) -> crate::LearnOutcome {
        learn_with_plan(
            codex_home,
            repo,
            LearnOptions {
                automation: AutomationMode::Local,
                local_test_time_budget_sec: 300,
            },
            LearnedPlan {
                prepare_steps: Vec::new(),
                fast_steps: vec![crate::step("ok", "true", StepPhase::Test)],
                full_steps: Vec::new(),
            },
        )
        .expect("learn")
    }

    fn current_cache_context(
        repo: &Path,
        manifest: &RepoCiManifest,
        mode: RunMode,
    ) -> (String, String, String) {
        let manifest_fingerprint = manifest_fingerprint(manifest);
        let worktree_fingerprint = worktree_fingerprint(repo).expect("worktree fingerprint");
        let key = cache_key(
            &manifest.repo_key,
            &manifest_fingerprint,
            &worktree_fingerprint,
            mode,
        );
        (key, manifest_fingerprint, worktree_fingerprint)
    }

    fn artifact_for(
        artifact_id: String,
        paths: &RepoCiPaths,
        manifest: &RepoCiManifest,
        mode: RunMode,
        status: RepoCiRunArtifactStatus,
        manifest_fingerprint: &str,
        worktree_fingerprint: &str,
    ) -> RepoCiRunArtifact {
        RepoCiRunArtifact {
            artifact_id,
            repo_root: paths.repo_root.clone(),
            repo_key: manifest.repo_key.clone(),
            state_dir: paths.state_dir.clone(),
            mode,
            status,
            exit_code: if status == RepoCiRunArtifactStatus::Passed {
                Some(0)
            } else {
                Some(1)
            },
            duration_ms: 1,
            started_at_unix_sec: 1,
            manifest_fingerprint: manifest_fingerprint.to_string(),
            worktree_fingerprint: worktree_fingerprint.to_string(),
            steps: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn write_artifact_file(paths: &RepoCiPaths, artifact: &RepoCiRunArtifact) {
        let path = run_artifact_path(paths, &artifact.artifact_id);
        fs::create_dir_all(path.parent().expect("artifact parent")).expect("artifact dir");
        fs::write(
            &path,
            serde_json::to_vec_pretty(artifact).expect("artifact json"),
        )
        .expect("write artifact");
    }

    fn write_sqlite_cache_entry(
        codex_home: &Path,
        key: &str,
        artifact: &RepoCiRunArtifact,
        cached_at_unix_sec: u64,
    ) {
        artifactory::put_cache_entry(
            codex_home,
            key,
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
