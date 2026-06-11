use super::*;
use crate::AutomationMode;
use crate::CapturedExitStatus;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::PathBuf;

fn repo_ci_step(id: &str, command: &str, phase: StepPhase) -> RepoCiStep {
    RepoCiStep {
        id: id.to_string(),
        command: command.to_string(),
        phase,
    }
}

fn captured_step(id: &str, event: CapturedStepEvent, exit_code: Option<i32>) -> CapturedStep {
    CapturedStep {
        id: id.to_string(),
        event,
        exit_code,
    }
}

#[test]
fn summarize_steps_returns_latest_status_sorted_by_id() {
    assert_eq!(
        summarize_steps(&[
            captured_step("b", CapturedStepEvent::Started, /*exit_code*/ None),
            captured_step("a", CapturedStepEvent::Started, /*exit_code*/ None),
            captured_step("b", CapturedStepEvent::Finished, Some(1)),
            captured_step("a", CapturedStepEvent::Finished, Some(0)),
        ]),
        vec![
            cicd_artifacts::StepStatus {
                id: "a".to_string(),
                status: cicd_artifacts::StepRunStatus::Passed,
                exit_code: Some(0),
            },
            cicd_artifacts::StepStatus {
                id: "b".to_string(),
                status: cicd_artifacts::StepRunStatus::Failed,
                exit_code: Some(1),
            },
        ]
    );
}

#[test]
fn store_captured_run_artifact_caches_passing_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let codex_home = temp.path().join("codex-home");
    let repo_root = temp.path().join("repo");
    fs::create_dir_all(&repo_root).expect("repo root");
    let paths = paths_for_repo(&codex_home, &repo_root).expect("paths");
    fs::create_dir_all(&paths.state_dir).expect("state dir");
    let repo_key = paths
        .state_dir
        .file_name()
        .expect("repo key")
        .to_string_lossy()
        .to_string();
    let manifest = RepoCiManifest {
        version: 1,
        repo_root: repo_root.clone(),
        repo_key,
        automation: AutomationMode::Local,
        local_test_time_budget_sec: 60,
        learned_at_unix_sec: 1,
        learning_sources: vec![SourceHash {
            path: PathBuf::from("Cargo.toml"),
            sha256: "a".repeat(64),
            kind: SourceKind::BuildManifest,
        }],
        prepare_steps: Vec::new(),
        fast_steps: vec![repo_ci_step("fast-test", "cargo test", StepPhase::Test)],
        full_steps: Vec::new(),
        validation: ValidationStatus::Passed {
            validated_at_unix_sec: 2,
        },
    };
    fs::write(
        &paths.manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("manifest json"),
    )
    .expect("write manifest");
    let run = CapturedRun {
        status: CapturedExitStatus {
            code: Some(0),
            success: true,
        },
        stdout: "ok\n".to_string(),
        stderr: String::new(),
        steps: vec![captured_step(
            "fast-test",
            CapturedStepEvent::Finished,
            Some(0),
        )],
        resource_usage: None,
    };

    let artifact = store_captured_run_artifact(
        &codex_home,
        &paths,
        &manifest,
        RunMode::Fast,
        &run,
        Duration::from_millis(25),
    )
    .expect("store artifact");
    let cached =
        lookup_cached_passing_run(&codex_home, &repo_root, RunMode::Fast).expect("lookup cache");

    assert_eq!(artifact.status, cicd_artifacts::RunArtifactStatus::Passed);
    assert_eq!(artifact.duration_ms, 25);
    assert_eq!(cached, Some(artifact));
}
