use anyhow::Context;
use anyhow::Result;
use serde_yaml::Mapping;
use serde_yaml::Value;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use crate::RepoCiStep;
use crate::inference;

/// Prompt scaffolding for AI-based repo CI discovery.
///
/// These hints are intentionally advisory. They surface repository-native
/// commands and workflow signals so the learner can assemble a better plan
/// without turning those hints into a deterministic result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoCiLearningHints {
    pub prepare_steps: Vec<RepoCiStep>,
    pub fast_steps: Vec<RepoCiStep>,
    pub full_steps: Vec<RepoCiStep>,
    pub workflow_run_hints: Vec<WorkflowRunHint>,
}

/// A concise GitHub Actions `run:` command annotated with its workflow/job
/// origin for AI-based repo CI prompt construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunHint {
    pub origin: String,
    pub command: String,
}

/// Collect prompt scaffolding for AI-based repo CI discovery.
pub fn collect_learning_hints(repo_root: &Path) -> Result<RepoCiLearningHints> {
    let (prepare_steps, fast_steps, full_steps) = inference::infer_steps(repo_root)?;
    Ok(RepoCiLearningHints {
        prepare_steps,
        fast_steps,
        full_steps,
        workflow_run_hints: collect_workflow_run_hints(repo_root)?,
    })
}

pub(crate) fn collect_workflow_run_hints(repo_root: &Path) -> Result<Vec<WorkflowRunHint>> {
    let workflow_dir = repo_root.join(".github/workflows");
    if !workflow_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut workflow_paths = fs::read_dir(&workflow_dir)?
        .map(|entry| entry.map(|value| value.path()))
        .collect::<Result<Vec<_>, _>>()?;
    workflow_paths.sort();

    let mut hints = Vec::new();
    let mut seen = HashSet::new();
    for workflow_path in workflow_paths {
        let Some(ext) = workflow_path.extension().and_then(OsStr::to_str) else {
            continue;
        };
        if !matches!(ext, "yml" | "yaml") {
            continue;
        }

        let relative = workflow_path
            .strip_prefix(repo_root)
            .unwrap_or(&workflow_path)
            .to_path_buf();
        hints.extend(extract_workflow_run_hints(
            &workflow_path,
            &relative,
            &mut seen,
        )?);
    }

    Ok(hints)
}

fn extract_workflow_run_hints(
    workflow_path: &Path,
    workflow_relative: &Path,
    seen: &mut HashSet<(String, String)>,
) -> Result<Vec<WorkflowRunHint>> {
    let contents = fs::read_to_string(workflow_path)
        .with_context(|| format!("failed to read {}", workflow_path.display()))?;
    let workflow: Value = serde_yaml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", workflow_path.display()))?;

    let Some(jobs) = workflow.get("jobs").and_then(Value::as_mapping) else {
        return Ok(Vec::new());
    };

    let mut hints = Vec::new();
    for (job_key, job_value) in jobs {
        let Some(job_id) = job_key.as_str() else {
            continue;
        };
        let Some(job) = job_value.as_mapping() else {
            continue;
        };
        let origin = workflow_job_origin(workflow_relative, job_id, job);
        let Some(steps) = mapping_get(job, "steps").and_then(Value::as_sequence) else {
            continue;
        };
        for step in steps {
            let Some(step_mapping) = step.as_mapping() else {
                continue;
            };
            let Some(run) = mapping_get(step_mapping, "run").and_then(Value::as_str) else {
                continue;
            };
            for command in concise_run_commands(run) {
                if seen.insert((origin.clone(), command.clone())) {
                    hints.push(WorkflowRunHint {
                        origin: origin.clone(),
                        command,
                    });
                }
            }
        }
    }

    Ok(hints)
}

fn workflow_job_origin(workflow_relative: &Path, job_id: &str, job: &Mapping) -> String {
    let workflow = workflow_relative.display();
    let name = mapping_get(job, "name")
        .and_then(Value::as_str)
        .unwrap_or(job_id);
    format!("{workflow}::{job_id} ({name})")
}

fn concise_run_commands(run: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut pending = String::new();

    for raw_line in run.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(prefix) = line.strip_suffix('\\') {
            pending.push_str(prefix.trim_end());
            pending.push(' ');
            continue;
        }

        let combined = if pending.is_empty() {
            line.to_string()
        } else {
            let combined = format!("{pending}{line}");
            pending.clear();
            combined
        };

        if is_concise_command_hint(&combined) {
            commands.push(combined);
        }
    }

    commands
}

fn is_concise_command_hint(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("if ")
        || trimmed == "then"
        || trimmed == "else"
        || trimmed == "fi"
        || trimmed.starts_with("for ")
        || trimmed == "do"
        || trimmed == "done"
        || trimmed.starts_with("while ")
        || trimmed.starts_with("[[")
        || trimmed.starts_with('[')
        || trimmed == "{"
        || trimmed == "}"
        || trimmed == "break"
        || trimmed == "continue"
        || trimmed == "return"
        || trimmed == "exit"
        || trimmed.starts_with("exit ")
        || trimmed.starts_with("echo ")
        || trimmed.starts_with("export ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("local ")
        || trimmed.starts_with("set ")
        || trimmed.starts_with("cd ")
        || trimmed.contains("&& continue")
        || trimmed.contains("&& break")
        || trimmed.contains("; continue")
        || trimmed.contains("; break")
        || trimmed.ends_with('{')
        || trimmed.contains("() {")
    {
        return false;
    }

    let first_token = trimmed.split_whitespace().next().unwrap_or_default();
    !first_token.is_empty() && !looks_like_shell_assignment(first_token)
}

fn looks_like_shell_assignment(token: &str) -> bool {
    token
        .split_once('=')
        .is_some_and(|(left, right)| !left.is_empty() && !right.is_empty())
}

fn mapping_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(Value::String(key.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepPhase;
    use pretty_assertions::assert_eq;

    #[test]
    fn collect_learning_hints_surfaces_repo_native_steps_and_workflows() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(temp.path().join(".github/workflows")).expect("workflow dir");
        fs::write(
            temp.path().join("Makefile"),
            "lint:\n\tmake lint\nbuild:\n\tmake build\ntest-unit:\n\tmake test-unit\ntest-integration-scylla:\n\tmake test-integration-scylla\n",
        )
        .expect("write Makefile");
        fs::write(temp.path().join("build.sbt"), "lazy val root = project\n").expect("build.sbt");
        fs::write(temp.path().join(".scalafmt.conf"), "version=3.0.0\n").expect("scalafmt");
        fs::write(
            temp.path().join(".github/workflows/tests.yml"),
            r#"
name: Tests
jobs:
  lint:
    name: Lint
    steps:
      - run: make lint
  unit:
    name: Unit tests
    steps:
      - run: make test-unit
  integration:
    name: Integration tests
    strategy:
      matrix:
        suite: [scylla]
    steps:
      - run: |
          make build
          make test-integration-scylla
"#,
        )
        .expect("workflow");

        let hints = collect_learning_hints(temp.path()).expect("collect hints");

        assert_eq!(
            hints.fast_steps,
            vec![
                crate::step("workflow-lint", "make lint", StepPhase::Lint),
                crate::step("workflow-test-unit", "make test-unit", StepPhase::Test),
                crate::step("workflow-build", "make build", StepPhase::Build),
            ]
        );
        assert!(hints.full_steps.contains(&crate::step(
            "workflow-test-integration",
            "make test-integration-scylla",
            StepPhase::Test,
        )));
        assert!(hints.workflow_run_hints.iter().any(|hint| {
            hint.origin.contains("tests.yml::lint") && hint.command == "make lint"
        }));
        assert!(hints.workflow_run_hints.iter().any(|hint| {
            hint.origin.contains("tests.yml::unit") && hint.command == "make test-unit"
        }));
        assert!(hints.workflow_run_hints.iter().any(|hint| {
            hint.origin.contains("tests.yml::integration") && hint.command == "make build"
        }));
    }

    #[test]
    fn concise_run_commands_filters_shell_scaffolding() {
        let commands = concise_run_commands(
            r#"
            export FOO=bar
            if true; then
              make lint
            fi
            VALUE=1
            make test-unit \
              EXTRA=1
            echo done
        "#,
        );

        assert_eq!(
            commands,
            vec![
                "make lint".to_string(),
                "make test-unit EXTRA=1".to_string()
            ]
        );
    }
}
