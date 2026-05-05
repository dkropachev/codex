use crate::repo_ci_exec::repo_ci_exec_timeout;
use crate::repo_ci_exec::run_repo_ci_exec_json;
use crate::repo_ci_exec::truncate_for_feedback;
use anyhow::Result;
use anyhow::anyhow;
use codex_repo_ci::AI_LEARN_MAX_ATTEMPTS;
use codex_repo_ci::LearnOptions;
use codex_repo_ci::LearnOutcome;
use codex_repo_ci::RepoCiAiLearnedPlan;
use codex_utils_cli::CliConfigOverrides;
use std::path::Path;
use std::time::Duration;

pub(crate) async fn learn_repo_ci_with_ai(
    root_config_overrides: &CliConfigOverrides,
    codex_home: &Path,
    cwd: &Path,
    options: LearnOptions,
) -> Result<LearnOutcome> {
    let repo_root = codex_repo_ci::repo_root_for_cwd(cwd)?;
    eprintln!("repo-ci learn: repository {}", repo_root.display());
    eprintln!("repo-ci learn: collecting repository and GitHub Actions hints");
    let learning_hints = codex_repo_ci::collect_learning_hints(&repo_root)?;
    let exec_timeout = repo_ci_exec_timeout(options.local_test_time_budget_sec);
    let mut prior_plan = None;
    let mut failure_feedback = None;

    eprintln!(
        "repo-ci learn: local validation budget {}s; AI discovery timeout {}s per attempt",
        options.local_test_time_budget_sec,
        exec_timeout.as_secs()
    );
    eprintln!(
        "repo-ci learn: collected hints: {} prepare, {} fast, {} full, {} workflow run commands, {} GitHub Actions timing hints",
        learning_hints.prepare_steps.len(),
        learning_hints.fast_steps.len(),
        learning_hints.full_steps.len(),
        learning_hints.workflow_run_hints.len(),
        learning_hints.workflow_history_hints.len()
    );
    if options.learning_instruction.is_some() {
        eprintln!("repo-ci learn: applying custom learner instruction");
    }

    for attempt in 1..=AI_LEARN_MAX_ATTEMPTS {
        eprintln!(
            "repo-ci learn: attempt {attempt}/{AI_LEARN_MAX_ATTEMPTS}: asking Codex to inspect the repo and generate a runner plan"
        );
        let prompt = codex_repo_ci::render_repo_ci_learning_prompt(
            &repo_root,
            &learning_hints,
            options.learning_instruction.as_deref(),
            options.local_test_time_budget_sec,
            attempt,
            prior_plan.as_ref(),
            failure_feedback.as_deref(),
        );
        let plan = run_exec_for_plan(
            root_config_overrides,
            &repo_root,
            &prompt,
            attempt,
            exec_timeout,
        )
        .await?;
        log_learned_plan(attempt, &plan);
        if let Some(guardrail_feedback) = codex_repo_ci::render_plan_guardrail_feedback(
            &plan,
            &learning_hints,
            options.local_test_time_budget_sec,
        ) {
            eprintln!(
                "repo-ci learn: attempt {attempt}: plan rejected before validation because it included a known-slow fast step"
            );
            eprintln!(
                "repo-ci learn: attempt {attempt}: {}",
                truncate_for_feedback(&guardrail_feedback.replace('\n', " "), 500)
            );
            failure_feedback = Some(guardrail_feedback);
            prior_plan = Some(plan);
            if attempt < AI_LEARN_MAX_ATTEMPTS {
                eprintln!(
                    "repo-ci learn: attempt {attempt}/{AI_LEARN_MAX_ATTEMPTS} failed; retrying with validation feedback"
                );
            }
            continue;
        }
        eprintln!(
            "repo-ci learn: attempt {attempt}/{AI_LEARN_MAX_ATTEMPTS}: writing runner and validating prepare/fast steps"
        );
        let outcome = codex_repo_ci::learn_with_plan(
            codex_home,
            &repo_root,
            options.clone(),
            plan.clone().into_learned_plan()?,
        )?;
        log_validation_outcome(attempt, &outcome);
        if matches!(
            outcome.manifest.validation,
            codex_repo_ci::ValidationStatus::Passed { .. }
        ) {
            eprintln!("repo-ci learn: validation passed on attempt {attempt}");
            return Ok(outcome);
        }

        failure_feedback = Some(codex_repo_ci::render_validation_feedback(&outcome)?);
        prior_plan = Some(plan);
        if attempt < AI_LEARN_MAX_ATTEMPTS {
            eprintln!(
                "repo-ci learn: attempt {attempt}/{AI_LEARN_MAX_ATTEMPTS} failed; retrying with validation feedback"
            );
        }
    }

    Err(anyhow!(
        "repo-ci learner could not produce a passing runner after {AI_LEARN_MAX_ATTEMPTS} attempts"
    ))
}

pub(crate) async fn normalize_repo_ci_learning_instruction_with_ai(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    instruction: &str,
    timeout: Duration,
) -> Result<String> {
    let prompt = codex_repo_ci::render_learning_instruction_validation_prompt(instruction);
    let validation: codex_repo_ci::RepoCiLearningInstructionValidation = run_repo_ci_exec_json(
        root_config_overrides,
        repo_root,
        &prompt,
        codex_repo_ci::learning_instruction_validation_schema(),
        "repo-ci learner instruction validation",
        timeout,
    )
    .await?;
    validation.into_instruction()
}

async fn run_exec_for_plan(
    root_config_overrides: &CliConfigOverrides,
    repo_root: &Path,
    prompt: &str,
    attempt: usize,
    timeout: Duration,
) -> Result<RepoCiAiLearnedPlan> {
    let action = format!("repo-ci learner attempt {attempt}/{AI_LEARN_MAX_ATTEMPTS}");
    run_repo_ci_exec_json(
        root_config_overrides,
        repo_root,
        prompt,
        codex_repo_ci::repo_ci_ai_plan_schema(),
        &action,
        timeout,
    )
    .await
}

fn log_learned_plan(attempt: usize, plan: &RepoCiAiLearnedPlan) {
    eprintln!(
        "repo-ci learn: attempt {attempt}: plan summary: {}",
        truncate_for_feedback(&plan.summary.replace('\n', " "), 240)
    );
    log_plan_steps("prepare", &plan.prepare_steps);
    log_plan_steps("fast", &plan.fast_steps);
    log_plan_steps("full", &plan.full_steps);
}

fn log_plan_steps(label: &str, steps: &[codex_repo_ci::RepoCiStep]) {
    if steps.is_empty() {
        eprintln!("repo-ci learn:   {label}: (none)");
        return;
    }

    eprintln!("repo-ci learn:   {label}:");
    for step in steps {
        eprintln!(
            "repo-ci learn:     - {} [{}]: {}",
            step.id,
            step_phase_label(&step.phase),
            step.command
        );
    }
}

fn log_validation_outcome(attempt: usize, outcome: &LearnOutcome) {
    let status = if outcome.validation_run.status.success {
        "passed"
    } else {
        "failed"
    };
    eprintln!(
        "repo-ci learn: attempt {attempt}: validation {status} in {} phase (exit {:?})",
        validation_phase_label(outcome.validation_phase),
        outcome.validation_exit_code
    );

    if outcome.validation_run.steps.is_empty() {
        eprintln!("repo-ci learn: attempt {attempt}: validation recorded no step events");
    } else {
        eprintln!("repo-ci learn: attempt {attempt}: validation step events:");
        for step in &outcome.validation_run.steps {
            eprintln!(
                "repo-ci learn:   - {} {} exit {:?}",
                step.id,
                captured_step_event_label(&step.event),
                step.exit_code
            );
        }
    }

    if !outcome.validation_run.status.success {
        log_validation_output_excerpt(attempt, "stdout", &outcome.validation_run.stdout);
        log_validation_output_excerpt(attempt, "stderr", &outcome.validation_run.stderr);
    }
}

fn log_validation_output_excerpt(attempt: usize, stream_name: &str, text: &str) {
    if text.trim().is_empty() {
        eprintln!("repo-ci learn: attempt {attempt}: validation {stream_name}: (empty)");
        return;
    }

    eprintln!(
        "repo-ci learn: attempt {attempt}: validation {stream_name}:\n{}",
        truncate_for_feedback(text, 4_000)
    );
}

fn step_phase_label(phase: &codex_repo_ci::StepPhase) -> &'static str {
    match phase {
        codex_repo_ci::StepPhase::Prepare => "prepare",
        codex_repo_ci::StepPhase::Lint => "lint",
        codex_repo_ci::StepPhase::Build => "build",
        codex_repo_ci::StepPhase::Test => "test",
    }
}

fn validation_phase_label(phase: codex_repo_ci::ValidationPhase) -> &'static str {
    match phase {
        codex_repo_ci::ValidationPhase::Prepare => "prepare",
        codex_repo_ci::ValidationPhase::Fast => "fast",
    }
}

fn captured_step_event_label(event: &codex_repo_ci::CapturedStepEvent) -> &'static str {
    match event {
        codex_repo_ci::CapturedStepEvent::Started => "started",
        codex_repo_ci::CapturedStepEvent::Finished => "finished",
    }
}
