use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_repo_ci::CapturedStep;
use codex_repo_ci::LearnOptions;
use codex_repo_ci::LearnOutcome;
use codex_repo_ci::LearnedPlan;
use codex_repo_ci::RepoCiStep;
use codex_repo_ci::StepPhase;
use codex_repo_ci::ValidationPhase;
use codex_utils_cli::CliConfigOverrides;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const MAX_LEARN_ATTEMPTS: usize = 5;
const MAX_FEEDBACK_BYTES: usize = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AiLearnedPlan {
    summary: String,
    prepare_steps: Vec<RepoCiStep>,
    fast_steps: Vec<RepoCiStep>,
    full_steps: Vec<RepoCiStep>,
}

impl AiLearnedPlan {
    fn into_learned_plan(mut self) -> Result<LearnedPlan> {
        self.prepare_steps = normalize_steps(self.prepare_steps, StepPhase::Prepare, "prepare");
        self.fast_steps = normalize_steps(self.fast_steps, StepPhase::Test, "fast");
        self.full_steps = normalize_steps(self.full_steps, StepPhase::Test, "full");

        if self.fast_steps.is_empty() {
            return Err(anyhow!("repo-ci learner produced no fast steps"));
        }
        if self.full_steps.is_empty() {
            self.full_steps = self.fast_steps.clone();
        }

        Ok(LearnedPlan {
            prepare_steps: self.prepare_steps,
            fast_steps: self.fast_steps,
            full_steps: self.full_steps,
        })
    }
}

pub(crate) async fn learn_repo_ci_with_ai(
    root_config_overrides: &CliConfigOverrides,
    codex_home: &Path,
    cwd: &Path,
    options: LearnOptions,
) -> Result<LearnOutcome> {
    let repo_root = codex_repo_ci::repo_root_for_cwd(cwd)?;
    let mut prior_plan = None;
    let mut failure_feedback = None;

    for attempt in 1..=MAX_LEARN_ATTEMPTS {
        let prompt = learn_prompt(
            &repo_root,
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

        failure_feedback = Some(render_validation_feedback(&outcome)?);
        prior_plan = Some(plan);
    }

    Err(anyhow!(
        "repo-ci learner could not produce a passing runner after {MAX_LEARN_ATTEMPTS} attempts"
    ))
}

fn learn_prompt(
    repo_root: &Path,
    local_test_time_budget_sec: u64,
    attempt: usize,
    prior_plan: Option<&AiLearnedPlan>,
    failure_feedback: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Learn local CI commands for this repository.\n\nRepository root: {}\nFast-step time budget: about {} seconds.\n\nYou must inspect the repository yourself to discover relevant files and commands.\nUse local read-only exploration only.\nDo not edit any files.\nDo not use the web.\nReturn strict JSON only matching the schema.\n\nInspection rules:\n- Use only non-interactive repository inspection commands.\n- Never launch an editor, pager, REPL, fuzzy finder, or any other interactive terminal UI.\n- Never run commands such as `$EDITOR`, `$VISUAL`, `vim`, `nvim`, `vi`, `nano`, `emacs`, `less`, `more`, `most`, `bat --paging`, `fzf`, or `top`.\n- Prefer commands such as `rg`, `find`, `ls`, `sed -n`, `cat`, `git show`, and similar non-interactive readers.\n\nRequirements:\n- Produce prepareSteps, fastSteps, and fullSteps.\n- Every command must run from the repository root via `bash -lc`.\n- Prefer project-native entry points such as `just`, `make`, package scripts, cargo commands, pytest, tox, or repo scripts.\n- `prepareSteps` should set up dependencies or caches only when truly needed.\n- `fastSteps` should be the quickest representative local checks that CI expects to stay green.\n- `fullSteps` may be broader than `fastSteps`, but must still be valid local commands.\n- Use stable, descriptive step ids.\n- Keep commands realistic for this machine; if a tool is optional and likely absent, prefer a repo wrapper or a more portable command.\n- If no distinct full suite exists, reuse the fast steps.\n",
        repo_root.display(),
        local_test_time_budget_sec,
    );
    if let Some(plan) = prior_plan
        && let Ok(plan_json) = serde_json::to_string_pretty(plan)
    {
        prompt.push_str("\nPrevious plan:\n```json\n");
        prompt.push_str(&plan_json);
        prompt.push_str("\n```\n");
    }
    if let Some(feedback) = failure_feedback {
        prompt.push_str(
            "\nThe previous plan failed validation. Repair it based on this output:\n```text\n",
        );
        prompt.push_str(feedback);
        prompt.push_str("\n```\n");
        prompt.push_str(
            "Preserve commands that already worked when possible, and only change what is needed to make validation pass.\n",
        );
    }
    prompt.push_str(&format!("\nThis is repair attempt {attempt}.\n"));
    prompt
}

async fn run_exec_for_plan(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    prompt: &str,
) -> Result<AiLearnedPlan> {
    let tempdir = tempfile::tempdir().context("failed to create tempdir for repo-ci learning")?;
    let schema_path = tempdir.path().join("repo-ci-plan.schema.json");
    let output_path = tempdir.path().join("repo-ci-plan.json");
    fs::write(
        &schema_path,
        serde_json::to_vec_pretty(&repo_ci_plan_schema())?,
    )
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

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to spawn repo-ci learner for {}",
            repo_root.display()
        )
    })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("repo-ci learner stdin was not available"))?;
    stdin
        .write_all(prompt.as_bytes())
        .await
        .context("failed to send prompt to repo-ci learner")?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .context("failed while waiting for repo-ci learner")?;
    if !output.status.success() {
        return Err(anyhow!(
            "repo-ci learner failed with {}: {}",
            output.status,
            truncate_for_feedback(&String::from_utf8_lossy(&output.stderr), MAX_FEEDBACK_BYTES),
        ));
    }

    let text = fs::read_to_string(&output_path)
        .with_context(|| format!("failed to read {}", output_path.display()))?;
    parse_json_payload(&text)
}

fn repo_ci_plan_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string" },
            "prepareSteps": steps_schema(),
            "fastSteps": steps_schema(),
            "fullSteps": steps_schema()
        },
        "required": ["summary", "prepareSteps", "fastSteps", "fullSteps"]
    })
}

fn steps_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string" },
                "command": { "type": "string" },
                "phase": {
                    "type": "string",
                    "enum": ["prepare", "lint", "build", "test"]
                }
            },
            "required": ["id", "command", "phase"]
        }
    })
}

fn normalize_steps(
    steps: Vec<RepoCiStep>,
    default_phase: StepPhase,
    prefix: &str,
) -> Vec<RepoCiStep> {
    let mut used_ids = BTreeSet::new();
    steps
        .into_iter()
        .enumerate()
        .filter_map(|(index, step)| {
            let command = step.command.trim().to_string();
            if command.is_empty() {
                return None;
            }

            let id = unique_step_id(
                if step.id.trim().is_empty() {
                    format!("{prefix}-{}", index + 1)
                } else {
                    step.id.trim().to_string()
                },
                &mut used_ids,
            );

            Some(RepoCiStep {
                id,
                command,
                phase: if prefix == "prepare" {
                    StepPhase::Prepare
                } else if matches!(step.phase, StepPhase::Prepare) {
                    default_phase.clone()
                } else {
                    step.phase
                },
            })
        })
        .collect()
}

fn unique_step_id(mut id: String, used_ids: &mut BTreeSet<String>) -> String {
    if used_ids.insert(id.clone()) {
        return id;
    }
    let base = id.clone();
    for suffix in 2.. {
        id = format!("{base}-{suffix}");
        if used_ids.insert(id.clone()) {
            return id;
        }
    }
    unreachable!("integer suffixes should eventually produce a unique repo-ci step id")
}

fn render_validation_feedback(outcome: &LearnOutcome) -> Result<String> {
    let phase = match outcome.validation_phase {
        ValidationPhase::Prepare => "prepare",
        ValidationPhase::Fast => "fast",
    };
    let plan_json = serde_json::to_string_pretty(&json!({
        "prepareSteps": outcome.manifest.prepare_steps,
        "fastSteps": outcome.manifest.fast_steps,
        "fullSteps": outcome.manifest.full_steps,
    }))?;
    let steps = render_captured_steps(&outcome.validation_run.steps);
    let feedback = format!(
        "Validation phase: {phase}\nExit code: {:?}\n\nCurrent plan:\n{plan_json}\n\nRecorded steps:\n{steps}\n\nstdout:\n{}\n\nstderr:\n{}",
        outcome.validation_exit_code,
        truncate_for_feedback(&outcome.validation_run.stdout, MAX_FEEDBACK_BYTES / 2),
        truncate_for_feedback(&outcome.validation_run.stderr, MAX_FEEDBACK_BYTES / 2),
    );
    Ok(truncate_for_feedback(&feedback, MAX_FEEDBACK_BYTES))
}

fn render_captured_steps(steps: &[CapturedStep]) -> String {
    if steps.is_empty() {
        return "(none)".to_string();
    }
    steps
        .iter()
        .map(|step| format!("{} {:?} {:?}", step.id, step.event, step.exit_code))
        .collect::<Vec<_>>()
        .join("\n")
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
    fn normalize_steps_fills_ids_and_drops_empty_commands() {
        let steps = normalize_steps(
            vec![
                RepoCiStep {
                    id: String::new(),
                    command: " cargo test ".to_string(),
                    phase: StepPhase::Test,
                },
                RepoCiStep {
                    id: String::new(),
                    command: " ".to_string(),
                    phase: StepPhase::Test,
                },
                RepoCiStep {
                    id: "run".to_string(),
                    command: "cargo fmt --check".to_string(),
                    phase: StepPhase::Lint,
                },
                RepoCiStep {
                    id: "run".to_string(),
                    command: "cargo clippy".to_string(),
                    phase: StepPhase::Lint,
                },
            ],
            StepPhase::Test,
            "fast",
        );

        assert_eq!(
            steps,
            vec![
                RepoCiStep {
                    id: "fast-1".to_string(),
                    command: "cargo test".to_string(),
                    phase: StepPhase::Test,
                },
                RepoCiStep {
                    id: "run".to_string(),
                    command: "cargo fmt --check".to_string(),
                    phase: StepPhase::Lint,
                },
                RepoCiStep {
                    id: "run-2".to_string(),
                    command: "cargo clippy".to_string(),
                    phase: StepPhase::Lint,
                },
            ]
        );
    }

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
