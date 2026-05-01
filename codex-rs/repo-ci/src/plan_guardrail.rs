use std::collections::BTreeSet;

use crate::RepoCiAiLearnedPlan;
use crate::RepoCiLearningHints;
use crate::RepoCiStep;
use crate::StepPhase;

pub fn render_plan_guardrail_feedback(
    plan: &RepoCiAiLearnedPlan,
    learning_hints: &RepoCiLearningHints,
    local_test_time_budget_sec: u64,
) -> Option<String> {
    let slow_history = slow_history_hints(learning_hints, local_test_time_budget_sec);
    if slow_history.is_empty() {
        return None;
    }

    let mut violations = Vec::new();
    for step in &plan.fast_steps {
        if let Some(history) = matching_slow_history(step, &slow_history) {
            violations.push(format!(
                "- {} [{}] `{}` matches slow GitHub Actions history `{}` ({} over {} sample(s), budget {}s)",
                step.id,
                phase_name(&step.phase),
                step.command,
                history.origin,
                format_duration(history.duration_seconds),
                history.sample_count,
                local_test_time_budget_sec,
            ));
        }
    }
    if violations.is_empty() {
        return None;
    }

    Some(format!(
        "The proposed plan was not executed because fastSteps included suite(s) that GitHub Actions history shows are too slow for local fast validation.\n{}\nMove these to fullSteps only if they are practical local commands, or omit them and rely on remote CI for those suites. Choose narrower local checks for fastSteps. Do not run integration, end-to-end, stress, cluster, service-backed, wheel-building, or packaging suites during discovery or fast validation.",
        violations.join("\n")
    ))
}

fn slow_history_hints(
    learning_hints: &RepoCiLearningHints,
    local_test_time_budget_sec: u64,
) -> Vec<&crate::WorkflowHistoryHint> {
    learning_hints
        .workflow_history_hints
        .iter()
        .filter(|hint| hint.duration_seconds > local_test_time_budget_sec)
        .filter(|hint| !slow_suite_tokens(&hint.origin).is_empty())
        .collect()
}

fn matching_slow_history<'a>(
    step: &RepoCiStep,
    slow_history: &'a [&crate::WorkflowHistoryHint],
) -> Option<&'a crate::WorkflowHistoryHint> {
    let step_text = normalized_words(&format!("{} {}", step.id, step.command));
    slow_history.iter().copied().find(|hint| {
        slow_suite_tokens(&hint.origin)
            .iter()
            .any(|token| step_text.contains(token))
    })
}

fn slow_suite_tokens(text: &str) -> BTreeSet<String> {
    normalized_words(text)
        .into_iter()
        .filter(|word| {
            matches!(
                word.as_str(),
                "integration"
                    | "integrations"
                    | "e2e"
                    | "endtoend"
                    | "stress"
                    | "cluster"
                    | "clusters"
                    | "service"
                    | "services"
                    | "wheel"
                    | "wheels"
                    | "packaging"
                    | "package"
                    | "packages"
            )
        })
        .collect()
}

fn normalized_words(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|word| {
            let word = word.trim().to_ascii_lowercase();
            (!word.is_empty()).then_some(word)
        })
        .collect()
}

fn phase_name(phase: &StepPhase) -> &'static str {
    match phase {
        StepPhase::Prepare => "prepare",
        StepPhase::Lint => "lint",
        StepPhase::Build => "build",
        StepPhase::Test => "test",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_known_slow_fast_step_before_validation() {
        let hints = RepoCiLearningHints {
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            workflow_run_hints: vec![],
            workflow_history_hints: vec![crate::WorkflowHistoryHint {
                origin: "Integration tests::test libev (3.11)".to_string(),
                conclusion: "success".to_string(),
                duration_seconds: 1_074,
                sample_count: 2,
                url: None,
            }],
        };
        let plan = RepoCiAiLearnedPlan {
            summary: "bad".to_string(),
            prepare_steps: vec![],
            fast_steps: vec![RepoCiStep {
                id: "integration".to_string(),
                command: "uv run pytest tests/integration/standard".to_string(),
                phase: StepPhase::Test,
            }],
            full_steps: vec![],
        };

        let feedback = render_plan_guardrail_feedback(&plan, &hints, 300)
            .expect("guardrail should reject the plan");

        assert!(feedback.contains("was not executed"));
        assert!(feedback.contains("Integration tests::test libev"));
        assert!(feedback.contains("uv run pytest tests/integration/standard"));
    }

    #[test]
    fn allows_unit_fast_step_with_slow_integration_history() {
        let hints = RepoCiLearningHints {
            prepare_steps: vec![],
            fast_steps: vec![],
            full_steps: vec![],
            workflow_run_hints: vec![],
            workflow_history_hints: vec![crate::WorkflowHistoryHint {
                origin: "Integration tests".to_string(),
                conclusion: "success".to_string(),
                duration_seconds: 1_100,
                sample_count: 1,
                url: None,
            }],
        };
        let plan = RepoCiAiLearnedPlan {
            summary: "ok".to_string(),
            prepare_steps: vec![],
            fast_steps: vec![RepoCiStep {
                id: "unit".to_string(),
                command: "uv run pytest tests/unit".to_string(),
                phase: StepPhase::Test,
            }],
            full_steps: vec![],
        };

        assert!(render_plan_guardrail_feedback(&plan, &hints, 300).is_none());
    }
}
