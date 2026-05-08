use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::path::Path;

use crate::CapturedStep;
use crate::LearnOutcome;
use crate::LearnedPlan;
use crate::RepoCiLearningHints;
use crate::RepoCiStep;
use crate::StepPhase;
use crate::ValidationPhase;

pub const AI_LEARN_MAX_ATTEMPTS: usize = 5;
const MAX_FEEDBACK_BYTES: usize = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoCiAiLearnedPlan {
    pub summary: String,
    pub prepare_steps: Vec<RepoCiStep>,
    pub fast_steps: Vec<RepoCiStep>,
    pub full_steps: Vec<RepoCiStep>,
}

impl RepoCiAiLearnedPlan {
    pub fn into_learned_plan(mut self) -> Result<LearnedPlan> {
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

pub fn render_repo_ci_learning_prompt(
    repo_root: &Path,
    learning_hints: &RepoCiLearningHints,
    local_test_time_budget_sec: u64,
    attempt: usize,
    prior_plan: Option<&RepoCiAiLearnedPlan>,
    failure_feedback: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Learn local CI commands for this repository.\n\nRepository root: {}\nFast-step time budget: about {} seconds.\n\nYou must inspect the repository yourself to discover relevant files and commands.\nUse local read-only exploration only.\nDo not edit any files.\nUse provided GitHub Actions history hints, and you may run read-only GitHub CLI metadata commands such as `gh run list`, `gh run view`, `gh workflow list`, `gh workflow view`, or read-only `gh api` GET requests.\nReturn strict JSON only matching the schema.\n\nInspection rules:\n- Use only non-interactive repository inspection commands.\n- Never launch an editor, pager, REPL, fuzzy finder, or any other interactive terminal UI.\n- Never run commands such as `$EDITOR`, `$VISUAL`, `vim`, `nvim`, `vi`, `nano`, `emacs`, `less`, `more`, `most`, `bat --paging`, `fzf`, or `top`.\n- During discovery, do not execute local test, build, install, package-manager, service, container, or cluster-starting commands. This includes commands such as `pytest`, `tox`, `nox`, `cargo test`, `make test`, `npm test`, `uv run pytest`, `pip install`, `uv sync`, `docker`, `docker compose`, `ccm`, and similar project runners.\n- Prefer commands such as `rg`, `find`, `ls`, `sed -n`, `cat`, `git show`, and similar non-interactive readers.\n\nRequirements:\n- Produce prepareSteps, fastSteps, and fullSteps.\n- Every command must run from the repository root via `bash -lc`.\n- Prefer project-native entry points such as `just`, `make`, package scripts, cargo commands, pytest, tox, or repo scripts.\n- `prepareSteps` should set up dependencies or caches only when truly needed.\n- `fastSteps` should be the quickest representative local checks that CI expects to stay green and fit within the fast-step budget.\n- Never put integration, end-to-end, stress, cluster, service-backed, wheel-building, packaging, or other long suites in fastSteps when GitHub Actions history shows they exceed the fast-step budget.\n- `fullSteps` may be broader than `fastSteps`, but must still be valid local commands; omit suites that are only practical in remote CI.\n- Do not include GitHub Actions-only housekeeping commands, artifact packaging/upload commands, commands that rely on `${{ ... }}` expressions, or commands that rely on GitHub runner variables such as `GITHUB_*`, `RUNNER_*`, or `ARTIFACT_TAR`.\n- Use stable, descriptive step ids.\n- Keep commands realistic for this machine; if a tool is optional and likely absent, prefer a repo wrapper or a more portable command.\n- If no distinct full suite exists, reuse the fast steps.\n",
        repo_root.display(),
        local_test_time_budget_sec,
    );
    prompt.push_str(&render_learning_hints(learning_hints));
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
            "Preserve verified-good commands when possible, and only narrow the failing part of the plan instead of replacing the whole plan.\n",
        );
    }
    prompt.push_str(&format!("\nThis is repair attempt {attempt}.\n"));
    prompt
}

pub fn repo_ci_ai_plan_schema() -> serde_json::Value {
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

pub fn render_validation_feedback(outcome: &LearnOutcome) -> Result<String> {
    let phase = match outcome.validation_phase {
        ValidationPhase::Prepare => "prepare",
        ValidationPhase::Fast => "fast",
    };
    let plan_json = serde_json::to_string_pretty(&json!({
        "prepareSteps": outcome.manifest.prepare_steps,
        "fastSteps": outcome.manifest.fast_steps,
        "fullSteps": outcome.manifest.full_steps,
    }))?;
    let step_summary = summarize_validation_steps(
        &validation_steps(outcome),
        &outcome.validation_run.steps,
        outcome.validation_exit_code,
    );
    let feedback = format!(
        "Validation phase: {phase}\nExit code: {:?}\n\nCurrent plan:\n{plan_json}\n\nValidation summary:\n{}\n\nstdout:\n{}\n\nstderr:\n{}",
        outcome.validation_exit_code,
        step_summary,
        truncate_for_feedback(&outcome.validation_run.stdout, MAX_FEEDBACK_BYTES / 2),
        truncate_for_feedback(&outcome.validation_run.stderr, MAX_FEEDBACK_BYTES / 2),
    );
    Ok(truncate_for_feedback(&feedback, MAX_FEEDBACK_BYTES))
}

fn render_learning_hints(learning_hints: &RepoCiLearningHints) -> String {
    let mut prompt = String::from("\nStrong repo signals:\n");
    prompt.push_str(&render_workflow_hint_section(
        &learning_hints.workflow_run_hints,
    ));
    prompt.push_str(&render_step_hint_section(
        "Inferred prepare-step candidates",
        &learning_hints.prepare_steps,
    ));
    prompt.push_str(&render_step_hint_section(
        "Inferred fast-step candidates",
        &learning_hints.fast_steps,
    ));
    prompt.push_str(&render_step_hint_section(
        "Inferred full-step candidates",
        &learning_hints.full_steps,
    ));
    prompt.push_str(&render_workflow_history_hint_section(
        &learning_hints.workflow_history_hints,
    ));
    prompt.push_str("\nPrompt rules for strong signals:\n");
    prompt.push_str(
        "- Treat checked-in CI workflow files, such as `.github/workflows/*.yml`, as the highest-priority source of CI/CD truth.\n",
    );
    prompt.push_str(
        "- If workflows conflict with AGENTS.md, AGENT.md, Makefiles, Justfiles, package scripts, docs, or checked-in repo scripts, follow the workflow commands and use the other files only to make those workflow commands runnable locally.\n",
    );
    prompt.push_str(
        "- When separate workflow or repo-native commands exist, keep `test-unit` in fastSteps and fullSteps, and keep `test-integration` and `test-e2e` as separate fullSteps. Do not collapse them into one generic test step.\n",
    );
    if has_repo_native_hints(learning_hints) {
        prompt.push_str(
            "- Do not replace discovered repo-native lint/test/build commands with generic fallback checks like `git diff --check` unless validation proves the repo-native commands are unusable.\n",
        );
    }
    prompt.push_str(
        "- Treat inferred candidate fastSteps as the default baseline unless validation proves they are unusable.\n",
    );
    prompt.push_str(
        "- Use workflow-only matrix expansion mainly to shape fullSteps, not to bloat fastSteps.\n",
    );
    if !learning_hints.workflow_history_hints.is_empty() {
        prompt.push_str(
            "- Treat GitHub Actions history as authoritative for runtime: workflows/jobs slower than the fast-step budget must be omitted from fastSteps, even if their commands appear in checked-in workflow files.\n",
        );
    }
    prompt
}

fn render_step_hint_section(title: &str, steps: &[RepoCiStep]) -> String {
    let mut rendered = format!("{title}:\n");
    if steps.is_empty() {
        rendered.push_str("- (none)\n");
        return rendered;
    }
    for step in steps {
        rendered.push_str(&format!(
            "- {} [{}] {}\n",
            step.id,
            phase_name(&step.phase),
            step.command,
        ));
    }
    rendered
}

fn render_workflow_hint_section(hints: &[crate::WorkflowRunHint]) -> String {
    let mut rendered = String::from("Workflow run hints:\n");
    if hints.is_empty() {
        rendered.push_str("- (none)\n");
        return rendered;
    }
    for hint in hints {
        rendered.push_str(&format!("- {} => {}\n", hint.origin, hint.command));
    }
    rendered
}

fn render_workflow_history_hint_section(hints: &[crate::WorkflowHistoryHint]) -> String {
    let mut rendered = String::from("GitHub Actions history timing hints:\n");
    if hints.is_empty() {
        rendered.push_str("- (none)\n");
        return rendered;
    }
    for hint in hints {
        let url = hint
            .url
            .as_deref()
            .map(|url| format!(" {url}"))
            .unwrap_or_default();
        rendered.push_str(&format!(
            "- {} => {} over {} sample(s), conclusion {}{}\n",
            hint.origin,
            format_duration(hint.duration_seconds),
            hint.sample_count,
            hint.conclusion,
            url,
        ));
    }
    rendered
}

fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    if minutes == 0 {
        format!("{seconds}s")
    } else if seconds == 0 {
        format!("{minutes}m")
    } else {
        format!("{minutes}m{seconds}s")
    }
}

fn has_repo_native_hints(learning_hints: &RepoCiLearningHints) -> bool {
    learning_hints
        .prepare_steps
        .iter()
        .chain(learning_hints.fast_steps.iter())
        .chain(learning_hints.full_steps.iter())
        .any(|step| step.command != "git diff --check")
        || !learning_hints.workflow_run_hints.is_empty()
}

fn phase_name(phase: &StepPhase) -> &'static str {
    match phase {
        StepPhase::Prepare => "prepare",
        StepPhase::Lint => "lint",
        StepPhase::Build => "build",
        StepPhase::Test => "test",
    }
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
            let is_github_actions_only =
                crate::learning_hints::is_github_actions_only_command(&command);
            if command.is_empty() || is_github_actions_only {
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

fn validation_steps(outcome: &LearnOutcome) -> Vec<RepoCiStep> {
    let mut steps = outcome.manifest.prepare_steps.clone();
    if matches!(outcome.validation_phase, ValidationPhase::Fast) {
        steps.extend(outcome.manifest.fast_steps.clone());
    }
    steps
}

fn summarize_validation_steps(
    validation_steps: &[RepoCiStep],
    captured_steps: &[CapturedStep],
    validation_exit_code: Option<i32>,
) -> String {
    let mut step_statuses = HashMap::new();
    for step in captured_steps {
        step_statuses.insert(step.id.clone(), (step.event.clone(), step.exit_code));
    }

    let mut passed = Vec::new();
    let mut failed = None;
    let mut not_run = Vec::new();
    let mut failure_seen = false;

    for step in validation_steps {
        match step_statuses.get(&step.id) {
            Some((_, Some(0))) if !failure_seen => passed.push(step_summary_line(step)),
            Some((event, exit_code)) if !failure_seen => {
                failed = Some(format!(
                    "{} (event: {:?}, exit_code: {:?}, validation exit code: {:?})",
                    step_summary_line(step),
                    event,
                    exit_code,
                    validation_exit_code,
                ));
                failure_seen = true;
            }
            _ => not_run.push(step_summary_line(step)),
        }
    }

    let mut summary = String::from("Passed steps:\n");
    if passed.is_empty() {
        summary.push_str("- (none)\n");
    } else {
        for step in passed {
            summary.push_str(&format!("- {step}\n"));
        }
    }

    summary.push_str("Failed step:\n");
    if let Some(failed) = failed {
        summary.push_str(&format!("- {failed}\n"));
    } else {
        summary.push_str("- (none)\n");
    }

    summary.push_str("Not-run remaining steps:\n");
    if not_run.is_empty() {
        summary.push_str("- (none)\n");
    } else {
        for step in not_run {
            summary.push_str(&format!("- {step}\n"));
        }
    }

    summary
}

fn step_summary_line(step: &RepoCiStep) -> String {
    format!("{} [{}] {}", step.id, phase_name(&step.phase), step.command)
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
    fn normalize_steps_drops_github_actions_only_commands() {
        let steps = normalize_steps(
            vec![
                RepoCiStep {
                    id: "artifact".to_string(),
                    command: r#"tar -cvf "$ARTIFACT_TAR" ."#.to_string(),
                    phase: StepPhase::Build,
                },
                RepoCiStep {
                    id: "matrix".to_string(),
                    command: "make test CASSANDRA_VERSION=${{ matrix.version }}".to_string(),
                    phase: StepPhase::Test,
                },
                RepoCiStep {
                    id: "lint".to_string(),
                    command: "make lint".to_string(),
                    phase: StepPhase::Lint,
                },
            ],
            StepPhase::Test,
            "fast",
        );

        assert_eq!(
            steps,
            vec![RepoCiStep {
                id: "lint".to_string(),
                command: "make lint".to_string(),
                phase: StepPhase::Lint,
            }]
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

    #[test]
    fn learn_prompt_includes_strong_repo_signals() {
        let hints = RepoCiLearningHints {
            prepare_steps: vec![],
            fast_steps: vec![
                RepoCiStep {
                    id: "make-lint".to_string(),
                    command: "make lint".to_string(),
                    phase: StepPhase::Lint,
                },
                RepoCiStep {
                    id: "make-test-unit".to_string(),
                    command: "make test-unit".to_string(),
                    phase: StepPhase::Test,
                },
            ],
            full_steps: vec![RepoCiStep {
                id: "make-build".to_string(),
                command: "make build".to_string(),
                phase: StepPhase::Build,
            }],
            workflow_run_hints: vec![crate::WorkflowRunHint {
                origin: ".github/workflows/tests.yml::lint (Lint)".to_string(),
                command: "make lint".to_string(),
            }],
            workflow_history_hints: vec![crate::WorkflowHistoryHint {
                origin: "Integration tests".to_string(),
                conclusion: "success".to_string(),
                duration_seconds: 1_100,
                sample_count: 3,
                url: Some("https://github.com/owner/repo/actions/runs/1".to_string()),
            }],
        };

        let prompt =
            render_repo_ci_learning_prompt(Path::new("/tmp/repo"), &hints, 120, 1, None, None);

        assert!(prompt.contains("Strong repo signals:"));
        let workflow_index = prompt.find("Workflow run hints:").expect("workflow hints");
        let inferred_index = prompt
            .find("Inferred fast-step candidates:")
            .expect("inferred hints");
        assert!(workflow_index < inferred_index);
        assert!(prompt.contains("make lint"));
        assert!(prompt.contains("make test-unit"));
        assert!(prompt.contains("make build"));
        assert!(prompt.contains("highest-priority source of CI/CD truth"));
        assert!(prompt.contains("AGENTS.md"));
        assert!(prompt.contains("test-integration"));
        assert!(prompt.contains("test-e2e"));
        assert!(prompt.contains("GitHub Actions history timing hints:"));
        assert!(prompt.contains("Integration tests => 18m20s over 3 sample(s)"));
        assert!(prompt.contains("do not execute local test, build, install"));
        assert!(prompt.contains("Do not replace discovered repo-native lint/test/build commands with generic fallback checks like `git diff --check` unless validation proves the repo-native commands are unusable."));
    }

    #[test]
    fn validation_feedback_separates_passed_failed_and_not_run_steps() {
        let outcome = LearnOutcome {
            paths: crate::RepoCiPaths {
                repo_root: Path::new("/tmp/repo").to_path_buf(),
                state_dir: Path::new("/tmp/state").to_path_buf(),
                manifest_path: Path::new("/tmp/state/manifest.json").to_path_buf(),
                runner_path: Path::new("/tmp/state/run_ci.sh").to_path_buf(),
            },
            manifest: crate::RepoCiManifest {
                version: 3,
                repo_root: Path::new("/tmp/repo").to_path_buf(),
                repo_key: "repo".to_string(),
                source_key: "source".to_string(),
                automation: crate::AutomationMode::Local,
                local_test_time_budget_sec: 120,
                learned_at_unix_sec: 0,
                learning_sources: vec![],
                inferred_issue_types: vec![],
                prepare_steps: vec![RepoCiStep {
                    id: "prepare".to_string(),
                    command: "make prepare".to_string(),
                    phase: StepPhase::Prepare,
                }],
                fast_steps: vec![
                    RepoCiStep {
                        id: "lint".to_string(),
                        command: "make lint".to_string(),
                        phase: StepPhase::Lint,
                    },
                    RepoCiStep {
                        id: "unit".to_string(),
                        command: "make test-unit".to_string(),
                        phase: StepPhase::Test,
                    },
                ],
                full_steps: vec![],
                validation: crate::ValidationStatus::Failed { exit_code: Some(2) },
            },
            validation_exit_code: Some(2),
            validation_phase: ValidationPhase::Fast,
            validation_run: crate::CapturedRun {
                status: crate::CapturedExitStatus {
                    code: Some(2),
                    success: false,
                },
                stdout: "lint failed".to_string(),
                stderr: String::new(),
                steps: vec![
                    CapturedStep {
                        id: "prepare".to_string(),
                        event: crate::CapturedStepEvent::Finished,
                        exit_code: Some(0),
                    },
                    CapturedStep {
                        id: "lint".to_string(),
                        event: crate::CapturedStepEvent::Finished,
                        exit_code: Some(2),
                    },
                ],
            },
        };

        let feedback = render_validation_feedback(&outcome).expect("feedback");

        assert!(feedback.contains("Passed steps:\n- prepare [prepare] make prepare"));
        assert!(feedback.contains("Failed step:\n- lint [lint] make lint"));
        assert!(feedback.contains("Not-run remaining steps:\n- unit [test] make test-unit"));
    }
}
