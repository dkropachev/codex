use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn brief_failure_includes_error_output_without_logs() {
    let artifact = failed_artifact("stdout details\n", "stderr failure\n");

    let response = format_run_artifact_response(
        "run",
        &artifact,
        DetailLevel::Brief,
        /*cache_hit*/ false,
    );

    assert_eq!(response["status"], json!("failed"));
    assert_eq!(response["error_output"], json!("stderr failure\n"));
    assert_eq!(response["error_output_truncated"], json!(false));
    assert_eq!(response["failed_step_ids"], json!(["test"]));
    assert_eq!(response.get("stdout"), None);
    assert_eq!(response.get("stderr"), None);
}

#[test]
fn brief_failure_uses_stdout_when_stderr_is_empty() {
    let artifact = failed_artifact("stdout failure\n", "");

    let response = format_run_artifact_response(
        "run",
        &artifact,
        DetailLevel::Brief,
        /*cache_hit*/ false,
    );

    assert_eq!(response["error_output"], json!("stdout failure\n"));
}

#[test]
fn detailed_failure_includes_bounded_logs() {
    let artifact = failed_artifact("stdout details\n", "stderr failure\n");

    let response = format_run_artifact_response(
        "run",
        &artifact,
        DetailLevel::Detailed,
        /*cache_hit*/ false,
    );

    assert_eq!(response["stdout"]["value"], json!("stdout details\n"));
    assert_eq!(response["stdout"]["truncated"], json!(false));
    assert_eq!(response["stderr"]["value"], json!("stderr failure\n"));
    assert_eq!(response["stderr"]["truncated"], json!(false));
}

#[test]
fn brief_cached_pass_omits_logs() {
    let artifact = passed_artifact("stdout details\n", "stderr details\n");

    let response = format_run_artifact_response(
        "run",
        &artifact,
        DetailLevel::Brief,
        /*cache_hit*/ true,
    );

    assert_eq!(response["status"], json!("passed"));
    assert_eq!(response["cache_hit"], json!(true));
    assert_eq!(response.get("stdout"), None);
    assert_eq!(response.get("stderr"), None);
    assert_eq!(response.get("error_output"), None);
}

fn failed_artifact(stdout: &str, stderr: &str) -> codex_repo_ci::RepoCiRunArtifact {
    codex_repo_ci::RepoCiRunArtifact {
        artifact_id: "artifact".to_string(),
        repo_root: PathBuf::from("/repo"),
        repo_key: "repo".to_string(),
        state_dir: PathBuf::from("/state"),
        mode: codex_repo_ci::RunMode::Fast,
        status: codex_repo_ci::RepoCiRunArtifactStatus::Failed,
        exit_code: Some(1),
        duration_ms: 42,
        started_at_unix_sec: 123,
        manifest_fingerprint: "manifest".to_string(),
        worktree_fingerprint: "worktree".to_string(),
        steps: vec![codex_repo_ci::RepoCiStepStatus {
            id: "test".to_string(),
            status: codex_repo_ci::RepoCiStepRunStatus::Failed,
            exit_code: Some(1),
        }],
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}

fn passed_artifact(stdout: &str, stderr: &str) -> codex_repo_ci::RepoCiRunArtifact {
    codex_repo_ci::RepoCiRunArtifact {
        status: codex_repo_ci::RepoCiRunArtifactStatus::Passed,
        exit_code: Some(0),
        steps: Vec::new(),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        ..failed_artifact("", "")
    }
}
