use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_repo_ci::AI_LEARN_MAX_ATTEMPTS;
use codex_repo_ci::LearnOptions;
use codex_repo_ci::LearnOutcome;
use codex_repo_ci::RepoCiAiLearnedPlan;
use codex_utils_cli::CliConfigOverrides;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const MAX_FEEDBACK_BYTES: usize = 16_000;

pub(crate) async fn learn_repo_ci_with_ai(
    root_config_overrides: &CliConfigOverrides,
    codex_home: &Path,
    cwd: &Path,
    options: LearnOptions,
) -> Result<LearnOutcome> {
    let repo_root = codex_repo_ci::repo_root_for_cwd(cwd)?;
    let learning_hints = codex_repo_ci::collect_learning_hints(&repo_root)?;
    let mut prior_plan = None;
    let mut failure_feedback = None;

    for attempt in 1..=AI_LEARN_MAX_ATTEMPTS {
        let prompt = codex_repo_ci::render_repo_ci_learning_prompt(
            &repo_root,
            &learning_hints,
            options.local_test_time_budget_sec,
            attempt,
            prior_plan.as_ref(),
            failure_feedback.as_deref(),
        );
        let plan = run_exec_for_plan(root_config_overrides, &repo_root, &prompt).await?;
        let outcome = codex_repo_ci::learn_with_plan(
            codex_home,
            &repo_root,
            options.clone(),
            plan.clone().into_learned_plan()?,
        )?;
        if matches!(
            outcome.manifest.validation,
            codex_repo_ci::ValidationStatus::Passed { .. }
        ) {
            return Ok(outcome);
        }

        failure_feedback = Some(codex_repo_ci::render_validation_feedback(&outcome)?);
        prior_plan = Some(plan);
    }

    Err(anyhow!(
        "repo-ci learner could not produce a passing runner after {AI_LEARN_MAX_ATTEMPTS} attempts"
    ))
}

async fn run_exec_for_plan(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    prompt: &str,
) -> Result<RepoCiAiLearnedPlan> {
    run_repo_ci_exec_json(
        root_config_overrides,
        repo_root,
        prompt,
        codex_repo_ci::repo_ci_ai_plan_schema(),
        "repo-ci learner",
    )
    .await
}

pub(crate) async fn run_repo_ci_exec_json<T>(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    prompt: &str,
    schema: serde_json::Value,
    action: &str,
) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let tempdir =
        tempfile::tempdir().with_context(|| format!("failed to create tempdir for {action}"))?;
    let schema_path = tempdir.path().join("repo-ci-output.schema.json");
    let output_path = tempdir.path().join("repo-ci-output.json");
    fs::write(&schema_path, serde_json::to_vec_pretty(&schema)?)
        .with_context(|| format!("failed to write {}", schema_path.display()))?;

    let current_exe = std::env::current_exe().context("failed to locate current codex binary")?;
    let mut command = Command::new(current_exe);
    command
        .arg("exec")
        .arg("--ephemeral")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-schema")
        .arg(&schema_path)
        .arg("--output-last-message")
        .arg(&output_path)
        .arg("-C")
        .arg(repo_root)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for raw_override in &root_config_overrides.raw_overrides {
        command.arg("--config").arg(raw_override);
    }
    command.arg("--config").arg("approval_policy=never");

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {action} for {}", repo_root.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("repo-ci learner stdin was not available"))?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .with_context(|| format!("failed to send prompt to {action}"))?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .with_context(|| format!("failed while waiting for {action}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{action} failed with {}: {}",
            output.status,
            truncate_for_feedback(&String::from_utf8_lossy(&output.stderr), MAX_FEEDBACK_BYTES),
        ));
    }

    let text = fs::read_to_string(&output_path)
        .with_context(|| format!("failed to read {}", output_path.display()))?;
    parse_json_payload(&text)
}

fn parse_json_payload<T>(text: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = text.trim();
    let json_text = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(trimmed);
    Ok(serde_json::from_str(json_text)?)
}

fn truncate_for_feedback(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let keep = max_bytes / 2;
    let head_end = floor_char_boundary(text, keep);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(keep));
    format!("{}\n...\n{}", &text[..head_end], &text[tail_start..])
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn truncate_for_feedback_keeps_ends() {
        let truncated = truncate_for_feedback("abcdefghij", 6);
        assert_eq!(truncated, "abc\n...\nhij");
    }

    #[test]
    fn truncate_for_feedback_handles_utf8_boundaries() {
        let truncated = truncate_for_feedback("abé🙂xyz", 7);
        assert_eq!(truncated, "ab\n...\nxyz");
    }
}
