use anyhow::Result;
use codex_cicd_artifacts as cicd_artifacts;
use serde_json::json;
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
use crate::RepoCiProgress;
use crate::RepoCiStep;
use crate::RunMode;
use crate::SourceHash;
use crate::SourceKind;
use crate::StepPhase;
use crate::ValidationStatus;
use crate::paths_for_repo;
use crate::read_manifest;
use crate::repo_root_for_cwd;
use crate::require_runner;
use crate::runner;
use crate::runner::RepoCiCancellation;

pub fn run_capture_persisted_with_cancellation(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
    cancellation: RepoCiCancellation,
) -> Result<cicd_artifacts::RunArtifact> {
    run_capture_persisted_with_cancellation_and_progress(
        codex_home,
        cwd,
        mode,
        cancellation,
        RepoCiProgress::none(),
    )
}

pub fn run_capture_persisted_with_cancellation_and_progress(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
    cancellation: RepoCiCancellation,
    progress: RepoCiProgress,
) -> Result<cicd_artifacts::RunArtifact> {
    cicd_artifacts::prune_stale_artifacts(codex_home)?;
    let paths = paths_for_repo(codex_home, cwd)?;
    require_runner(&paths)?;
    let manifest = read_manifest(&paths.manifest_path)?;
    register_manifest_artifact_state(codex_home, &paths, &manifest)?;
    cicd_artifacts::record_artifact_hit(codex_home, &paths.state_dir)?;
    let started = Instant::now();
    let run = runner::capture_runner_with_progress(
        &paths,
        mode.as_str(),
        manifest.local_test_time_budget_sec,
        &cancellation,
        progress,
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
) -> Result<cicd_artifacts::RunArtifact> {
    register_manifest_artifact_state(codex_home, paths, manifest)?;
    let manifest_fingerprint = manifest_fingerprint(manifest);
    let worktree_fingerprint = worktree_fingerprint(&paths.repo_root)?;
    let artifact_mode = artifact_mode(mode);
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let artifact_id = cicd_artifacts::artifact_id(
        &manifest.repo_key,
        &manifest_fingerprint,
        &worktree_fingerprint,
        artifact_mode,
        started_at.as_nanos(),
    );
    let artifact = cicd_artifacts::RunArtifact {
        artifact_id,
        repo_root: paths.repo_root.clone(),
        repo_key: manifest.repo_key.clone(),
        state_dir: paths.state_dir.clone(),
        mode: artifact_mode,
        status: if run.status.success {
            cicd_artifacts::RunArtifactStatus::Passed
        } else {
            cicd_artifacts::RunArtifactStatus::Failed
        },
        exit_code: run.status.code,
        duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        started_at_unix_sec: started_at.as_secs(),
        manifest_fingerprint,
        worktree_fingerprint,
        steps: summarize_steps(&run.steps),
        resource_usage: run.resource_usage.clone(),
        stdout: run.stdout.clone(),
        stderr: run.stderr.clone(),
    };
    cicd_artifacts::write_run_artifact(codex_home, &paths.state_dir, &artifact)?;
    if artifact.status == cicd_artifacts::RunArtifactStatus::Passed {
        cicd_artifacts::put_passing_run_cache(codex_home, &artifact)?;
    }
    cicd_artifacts::record_artifact_hit(codex_home, &paths.state_dir)?;
    Ok(artifact)
}

pub fn lookup_cached_passing_run(
    codex_home: &Path,
    cwd: &Path,
    mode: RunMode,
) -> Result<Option<cicd_artifacts::RunArtifact>> {
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
        mode: artifact_mode(mode),
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

fn artifact_mode(mode: RunMode) -> cicd_artifacts::RunMode {
    match mode {
        RunMode::Fast => cicd_artifacts::RunMode::Fast,
        RunMode::Full => cicd_artifacts::RunMode::Full,
    }
}

fn register_manifest_artifact_state(
    codex_home: &Path,
    paths: &RepoCiPaths,
    manifest: &RepoCiManifest,
) -> Result<()> {
    let sources = manifest_artifact_sources(manifest);
    let source_key = cicd_artifacts::source_key(&sources);
    cicd_artifacts::register_state(
        codex_home,
        &manifest.repo_key,
        &source_key,
        &paths.state_dir,
        &sources,
        json!({
            "repoRoot": paths.repo_root,
            "manifestFingerprint": manifest_fingerprint(manifest),
            "automation": manifest.automation.as_str(),
            "localTestTimeBudgetSec": manifest.local_test_time_budget_sec,
        }),
    )
}

fn manifest_artifact_sources(manifest: &RepoCiManifest) -> Vec<cicd_artifacts::ArtifactSource> {
    manifest
        .learning_sources
        .iter()
        .map(|source| {
            cicd_artifacts::ArtifactSource::new(
                source.path.clone(),
                source_kind_name(&source.kind),
                source.sha256.clone(),
            )
        })
        .collect()
}

fn source_kind_name(kind: &SourceKind) -> &'static str {
    match kind {
        SourceKind::CiWorkflow => "ci_workflow",
        SourceKind::BuildManifest => "build_manifest",
        SourceKind::Lockfile => "lockfile",
        SourceKind::Tooling => "tooling",
    }
}

fn hash_sources(hasher: &mut Sha256, sources: &[SourceHash]) {
    hasher.update(b"\nsources\0");
    for source in sources {
        hasher.update(b"path\0");
        hasher.update(source.path.to_string_lossy().as_bytes());
        hasher.update(b"\0kind\0");
        hasher.update(source_kind_name(&source.kind).as_bytes());
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

fn summarize_steps(steps: &[CapturedStep]) -> Vec<cicd_artifacts::StepStatus> {
    let mut by_id = BTreeMap::<String, cicd_artifacts::StepStatus>::new();
    for step in steps {
        let status = match step.event {
            CapturedStepEvent::Started => cicd_artifacts::StepRunStatus::Started,
            CapturedStepEvent::Finished => {
                if step.exit_code == Some(0) {
                    cicd_artifacts::StepRunStatus::Passed
                } else {
                    cicd_artifacts::StepRunStatus::Failed
                }
            }
        };
        by_id.insert(
            step.id.clone(),
            cicd_artifacts::StepStatus {
                id: step.id.clone(),
                status,
                exit_code: step.exit_code,
            },
        );
    }
    by_id.into_values().collect()
}

#[cfg(test)]
#[path = "persisted_runs_tests.rs"]
mod tests;
