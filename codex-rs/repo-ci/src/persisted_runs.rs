use anyhow::Result;
use codex_cicd_artifacts as cicd_artifacts;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::CapturedRun;
use crate::CapturedStep;
use crate::CapturedStepEvent;
use crate::RepoCiManifest;
use crate::RepoCiPaths;
use crate::RepoCiRunArtifact;
use crate::RepoCiRunArtifactStatus;
use crate::RepoCiStep;
use crate::RepoCiStepRunStatus;
use crate::RepoCiStepStatus;
use crate::RunMode;
use crate::SourceHash;
use crate::SourceKind;
use crate::StepPhase;
use crate::ValidationStatus;
use crate::paths_for_repo;
use crate::read_manifest;
use crate::register_manifest_artifact_state;
use crate::repo_root_for_cwd;
use crate::require_runner;
use crate::runner::RepoCiCancellation;
use crate::runner::capture_runner;
use crate::touch_manifest_artifact_state;

pub fn run_capture_persisted_with_cancellation(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
    cancellation: RepoCiCancellation,
) -> Result<RepoCiRunArtifact> {
    cicd_artifacts::prune_stale_artifacts(codex_home)?;
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    touch_manifest_artifact_state(codex_home, &paths, &manifest)?;
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
    register_manifest_artifact_state(codex_home, paths, manifest)?;
    let manifest_fingerprint = manifest_fingerprint(manifest);
    let worktree_fingerprint = worktree_fingerprint(&paths.repo_root)?;
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let artifact_id = cicd_artifacts::artifact_id(
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
        started_at_unix_sec: started_at.as_secs(),
        manifest_fingerprint,
        worktree_fingerprint,
        steps: summarize_steps(&run.steps),
        stdout: run.stdout.clone(),
        stderr: run.stderr.clone(),
    };
    cicd_artifacts::write_run_artifact(codex_home, &paths.state_dir, &artifact)?;
    if artifact.status == RepoCiRunArtifactStatus::Passed {
        cicd_artifacts::put_passing_run_cache(codex_home, &artifact)?;
    }
    cicd_artifacts::record_artifact_hit(codex_home, &paths.state_dir)?;
    Ok(artifact)
}

pub fn lookup_cached_passing_run(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
) -> Result<Option<RepoCiRunArtifact>> {
    let paths = paths_for_repo(codex_home, cwd)?;
    if !paths.manifest_path.exists() {
        return Ok(None);
    }
    let manifest = read_manifest(&paths.manifest_path)?;
    let manifest_fingerprint = manifest_fingerprint(&manifest);
    let context = cicd_artifacts::RunCacheContext {
        repo_key: manifest.repo_key,
        manifest_fingerprint,
        worktree_fingerprint: worktree_fingerprint(cwd)?,
        mode,
    };
    cicd_artifacts::lookup_cached_passing_run(codex_home, &context)
}

pub fn worktree_fingerprint(cwd: &Path) -> Result<String> {
    let repo_root = repo_root_for_cwd(cwd)?;
    cicd_artifacts::worktree_fingerprint_for_repo(&repo_root)
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

fn hash_steps(hasher: &mut Sha256, label: &str, steps: &[RepoCiStep]) {
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
