use super::DetailLevel;
use crate::function_tool::FunctionCallError;
use serde::Serialize;
use serde_json::json;

const BRIEF_ERROR_OUTPUT_MAX_BYTES: usize = 4_000;
pub(super) const DETAILED_LOG_MAX_BYTES: usize = 64_000;

pub(super) fn format_run_artifact_response(
    operation: &str,
    artifact: &codex_repo_ci::RepoCiRunArtifact,
    detail: DetailLevel,
    cache_hit: bool,
) -> serde_json::Value {
    let status = match artifact.status {
        codex_repo_ci::RepoCiRunArtifactStatus::Passed => "passed",
        codex_repo_ci::RepoCiRunArtifactStatus::Failed => "failed",
    };
    let mut output = artifact_metadata_json(artifact);
    if let Some(object) = output.as_object_mut() {
        object.insert("operation".to_string(), json!(operation));
        object.insert("status".to_string(), json!(status));
        object.insert("cache_hit".to_string(), json!(cache_hit));
        if artifact.status == codex_repo_ci::RepoCiRunArtifactStatus::Failed {
            let error_output = compact_error_output(artifact);
            object.insert("error_output".to_string(), json!(&error_output.value));
            object.insert(
                "error_output_truncated".to_string(),
                json!(error_output.truncated),
            );
            object.insert(
                "failed_step_ids".to_string(),
                json!(failed_step_ids(artifact)),
            );
        }
        if detail == DetailLevel::Detailed {
            object.insert(
                "stdout".to_string(),
                json!(bounded_log(&artifact.stdout, None, DETAILED_LOG_MAX_BYTES)),
            );
            object.insert(
                "stderr".to_string(),
                json!(bounded_log(&artifact.stderr, None, DETAILED_LOG_MAX_BYTES)),
            );
        }
    }
    output
}

pub(super) fn artifact_metadata_json(
    artifact: &codex_repo_ci::RepoCiRunArtifact,
) -> serde_json::Value {
    json!({
        "artifact_id": &artifact.artifact_id,
        "repo_root": &artifact.repo_root,
        "mode": artifact.mode,
        "status": artifact.status,
        "exit_code": artifact.exit_code,
        "duration_ms": artifact.duration_ms,
        "steps": &artifact.steps,
        "manifest_fingerprint": &artifact.manifest_fingerprint,
        "worktree_fingerprint": &artifact.worktree_fingerprint,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct BoundedText {
    value: String,
    truncated: bool,
}

fn compact_error_output(artifact: &codex_repo_ci::RepoCiRunArtifact) -> BoundedText {
    let candidate = if artifact.stderr.trim().is_empty() {
        artifact.stdout.as_str()
    } else {
        artifact.stderr.as_str()
    };
    bounded_log(candidate, Some(80), BRIEF_ERROR_OUTPUT_MAX_BYTES)
}

pub(super) fn bounded_log(value: &str, tail_lines: Option<usize>, max_bytes: usize) -> BoundedText {
    let (tailed, truncated_by_lines) = tail_lines.map_or_else(
        || (value.to_string(), false),
        |tail_lines| {
            let lines = value.lines().collect::<Vec<_>>();
            if lines.len() <= tail_lines {
                return (value.to_string(), false);
            }
            let start = lines.len().saturating_sub(tail_lines);
            (lines[start..].join("\n"), true)
        },
    );
    if tailed.len() <= max_bytes {
        return BoundedText {
            value: tailed,
            truncated: truncated_by_lines,
        };
    }
    let start = tailed
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= tailed.len().saturating_sub(max_bytes))
        .unwrap_or(tailed.len());
    BoundedText {
        value: tailed[start..].to_string(),
        truncated: true,
    }
}

fn failed_step_ids(artifact: &codex_repo_ci::RepoCiRunArtifact) -> Vec<String> {
    artifact
        .steps
        .iter()
        .filter(|step| step.status == codex_repo_ci::RepoCiStepRunStatus::Failed)
        .map(|step| step.id.clone())
        .collect()
}

pub(super) fn select_step_output(output: &str, step_id: Option<&str>) -> String {
    let Some(step_id) = step_id else {
        return output.to_string();
    };
    let marker = format!("==> {step_id}");
    let mut selected = Vec::new();
    let mut in_step = false;
    for line in output.lines() {
        if line == marker {
            in_step = true;
            selected.push(line);
            continue;
        }
        if in_step && line.starts_with("==> ") {
            break;
        }
        if in_step {
            selected.push(line);
        }
    }
    selected.join("\n")
}

pub(super) fn source_json(source: &codex_repo_ci::SourceHash) -> serde_json::Value {
    json!({
        "path": &source.path,
        "sha256": &source.sha256,
        "kind": &source.kind,
    })
}

pub(super) fn json_output(value: serde_json::Value) -> Result<String, FunctionCallError> {
    serde_json::to_string_pretty(&value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to serialize repo_ci output: {err}"))
    })
}
