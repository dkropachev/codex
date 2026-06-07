use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use codex_config::types::WorkflowsConfigToml;

use crate::registry::WorkflowSummary;
use crate::registry::discover_workflows;
use crate::registry::workflow_git_status;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_runner::WorkflowValidationReport;
use crate::validation_runner::run_validation_command;
use crate::validation_runner::validation_report_message;
use crate::workflow_api::validate_and_publish_workflow_api;
use crate::workflow_api::validate_workflow_api_contract;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowQualityHookFeedback {
    pub reason: String,
    pub additional_context: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowQualityFailure {
    workflow: WorkflowSummary,
    findings: Vec<WorkflowQualityFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowQualityFinding {
    rule_id: &'static str,
    title: &'static str,
    path: PathBuf,
    body: String,
}

pub fn workflow_quality_feedback(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Option<WorkflowQualityHookFeedback>> {
    let workflows = discover_workflows(codex_home, cwd, config)?;
    let mut failures = Vec::new();

    for workflow in workflows {
        if let Some(failure) = workflow_quality_failure_for_workflow(&workflow)? {
            failures.push(failure);
        }

        let report = validate_and_publish_workflow_api(
            codex_home,
            cwd,
            config,
            &workflow,
            run_validation_command,
        )?;
        if report.status == crate::registry::WorkflowValidationStatus::Valid {
            continue;
        }

        failures.push(WorkflowQualityFailure {
            findings: findings_for_report(&workflow, &report),
            workflow,
        });
    }

    if failures.is_empty() {
        Ok(None)
    } else {
        Ok(Some(WorkflowQualityHookFeedback {
            reason: render_findings(&failures, /*include_guidance*/ false),
            additional_context: render_findings(&failures, /*include_guidance*/ true),
        }))
    }
}

pub fn workflow_quality_block_reason(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Option<String>> {
    Ok(workflow_quality_feedback(codex_home, cwd, config)?.map(|feedback| feedback.reason))
}

pub(crate) fn workflow_quality_feedback_for_workflow(
    workflow: &WorkflowSummary,
) -> Result<Option<WorkflowQualityHookFeedback>> {
    Ok(
        workflow_quality_failure_for_workflow(workflow)?.map(|failure| {
            WorkflowQualityHookFeedback {
                reason: render_findings(
                    std::slice::from_ref(&failure),
                    /*include_guidance*/ false,
                ),
                additional_context: render_findings(
                    std::slice::from_ref(&failure),
                    /*include_guidance*/ true,
                ),
            }
        }),
    )
}

pub(crate) fn workflow_quality_block_reason_for_workflow(
    workflow: &WorkflowSummary,
) -> Result<Option<String>> {
    Ok(workflow_quality_feedback_for_workflow(workflow)?.map(|feedback| feedback.reason))
}

#[cfg(test)]
use anyhow::anyhow;
#[cfg(test)]
pub(crate) fn workflow_quality_block_reason_for_path(
    codex_home: &Path,
    cwd: &Path,
    config: &WorkflowsConfigToml,
    workflow_path: &Path,
) -> Result<Option<String>> {
    let workflow = discover_workflows(codex_home, cwd, config)?
        .into_iter()
        .find(|workflow| workflow.path == workflow_path)
        .ok_or_else(|| anyhow!("workflow at {} was not found", workflow_path.display()))?;

    workflow_quality_block_reason_for_workflow(&workflow)
}

fn workflow_quality_failure_for_workflow(
    workflow: &WorkflowSummary,
) -> Result<Option<WorkflowQualityFailure>> {
    if workflow_git_status(workflow).is_empty() {
        return Ok(None);
    }

    let report = validate_workflow_api_contract(workflow, run_validation_command)?;
    if report.status == crate::registry::WorkflowValidationStatus::Valid {
        return Ok(None);
    }

    Ok(Some(WorkflowQualityFailure {
        findings: findings_for_report(workflow, &report),
        workflow: workflow.clone(),
    }))
}

fn findings_for_report(
    workflow: &WorkflowSummary,
    report: &WorkflowValidationReport,
) -> Vec<WorkflowQualityFinding> {
    let mut findings = report
        .findings
        .iter()
        .map(|finding| finding_for_validation_finding(workflow, finding))
        .collect::<Vec<_>>();
    if findings.is_empty() {
        findings.push(WorkflowQualityFinding {
            rule_id: "WF-011",
            title: "Workflow validation reported a failure",
            path: workflow.path.clone(),
            body: validation_report_message(report),
        });
    }
    findings
}

fn finding_for_validation_finding(
    workflow: &WorkflowSummary,
    finding: &WorkflowValidationFinding,
) -> WorkflowQualityFinding {
    let body = if matches!(
        finding,
        WorkflowValidationFinding::ValidationCommandFailed { .. }
    ) {
        render_command_failure_body(finding)
    } else {
        finding.message()
    };
    WorkflowQualityFinding {
        rule_id: finding.rule_id(),
        title: finding.title(),
        path: finding.resolved_primary_path(&workflow.path),
        body,
    }
}

fn render_command_failure_body(finding: &WorkflowValidationFinding) -> String {
    let WorkflowValidationFinding::ValidationCommandFailed {
        command,
        exit_code,
        stdout,
        stderr,
    } = finding
    else {
        return finding.message();
    };

    let mut lines = vec![finding.message(), format!("Command: `{command}`")];
    let exit_label = exit_code
        .map(|code| format!("Exit status: {code}"))
        .unwrap_or_else(|| "Exit status: non-zero".to_string());
    lines.push(exit_label);

    if let Some(stderr) = trimmed_output_snippet(stderr) {
        lines.push("stderr:".to_string());
        lines.extend(stderr.lines().map(ToString::to_string));
    } else if let Some(stdout) = trimmed_output_snippet(stdout) {
        lines.push("stdout:".to_string());
        lines.extend(stdout.lines().map(ToString::to_string));
    }

    lines.join("\n")
}

fn trimmed_output_snippet(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines().take(12).collect::<Vec<_>>();
    let had_more = trimmed.lines().count() > lines.len();
    if had_more {
        lines.push("...");
    }
    let snippet = lines.join("\n");
    if snippet.chars().count() > 1200 {
        let truncated = snippet.chars().take(1200).collect::<String>();
        Some(format!("{truncated}..."))
    } else {
        Some(snippet)
    }
}

fn render_findings(failures: &[WorkflowQualityFailure], include_guidance: bool) -> String {
    let mut lines = vec!["Workflow quality findings:".to_string()];

    for failure in failures {
        lines.push(String::new());
        lines.push(format!(
            "Workflow `{}` ({})",
            failure.workflow.id,
            failure.workflow.path.display()
        ));
        for finding in &failure.findings {
            lines.push(format!(
                "- [{}] {} — {}",
                finding.rule_id,
                finding.title,
                finding.path.display()
            ));
            for body_line in finding.body.lines() {
                lines.push(format!("  {body_line}"));
            }
        }
    }

    if include_guidance {
        lines.push(String::new());
        lines.push(
            "Fix these findings in the current workflow development cycle before treating the workflow as complete. Keep src/workflow.ts as the TypeScript API contract source of truth, and keep README.md, package.json, tsconfig.json, workflow.yaml, tests, contract smoke, and dependency metadata aligned with the settled DESIGN.md."
                .to_string(),
        );
        lines.push(
            "If the right fix requires a design change, raise a DESIGN.md request under WF-015 instead of editing DESIGN.md directly."
                .to_string(),
        );
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::workflow_quality_block_reason;
    use super::workflow_quality_feedback;
    use codex_config::types::WorkflowsConfigToml;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn write_workflow_fixture(root: &Path, id: &str) -> PathBuf {
        let workflow_dir = root.join("workflows").join(id);
        fs::create_dir_all(workflow_dir.join("src/tests")).expect("create src/tests");
        fs::create_dir_all(workflow_dir.join("state")).expect("create state");
        fs::write(
            workflow_dir.join(".gitignore"),
            "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
        )
        .expect("write .gitignore");
        fs::write(
            workflow_dir.join("README.md"),
            "# Test\n\n## Usage\n\nRun `/fix`.\n\n## Workflow Runtime\n\nRuns as a local TypeScript workflow package.\n\n## Dependencies\n\nUses local package dependencies only.\n\n## Validation\n\nRuns build and test commands.\n\n## Maintenance\n\nKeep docs, metadata, and tests aligned.\n",
        )
        .expect("write README");
        fs::write(
            workflow_dir.join("DESIGN.md"),
            "# Test Design\n\n## Overview\n\nTest workflow fixture.\n\n## Architecture\n\nSource lives under src/.\n\n## Data Flow\n\nThe workflow returns a JSON result.\n\n## Failure Handling\n\nValidation commands report failures.\n\n## Recovery Behavior\n\nNo recovery behavior.\n\n## Test Matrix\n\nPositive, negative, load, and autocomplete tests.\n\n## Maintenance Notes\n\nKeep validation metadata current.\n",
        )
        .expect("write DESIGN");
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "codex-workflow-test",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk",
    "test": "bun test src/tests",
    "run": "bun src/workflow.ts"
  },
  "devDependencies": {
    "@types/node": "1.0.0",
    "typescript": "1.0.0"
  }
}
"#,
        )
        .expect("write package.json");
        fs::write(
            workflow_dir.join("tsconfig.json"),
            "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n",
        )
        .expect("write tsconfig");
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport default async function workflow(_ctx: unknown, _input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true }; }\nexport async function complete() { return []; }\n",
        )
        .expect("write workflow");
        fs::write(
            workflow_dir.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {}\n",
        )
        .expect("write positive test");
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nimport \"../workflow.ts\"\n",
        )
        .expect("write load test");
        fs::write(
            workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
            "// workflow-covers: autocomplete\nexport {}\n",
        )
        .expect("write autocomplete test");
        fs::write(
            workflow_dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {}\n",
        )
        .expect("write negative test");
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nimport \"../workflow.ts\"\n",
        )
        .expect("write load test");
        fs::write(
            workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
            "// workflow-covers: autocomplete\nexport {}\n",
        )
        .expect("write autocomplete test");
        fs::create_dir_all(workflow_dir.join(".git")).expect("create git dir");
        fs::write(
            workflow_dir.join("workflow.yaml"),
            "id: review/fix\ndependencies:\n  runtime: []\n  development:\n    - '@types/node'\n    - typescript\nvalidation:\n  commands:\n    - bun build src/workflow.ts --target=bun --outdir artifacts/build --external @openai/codex-sdk\n    - bun test src/tests\n  contractSmoke:\n    input: {}\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
        )
        .expect("write workflow spec");

        let status = Command::new("git")
            .args(["init"])
            .current_dir(&workflow_dir)
            .output()
            .expect("git init");
        assert!(status.status.success(), "git init should succeed");
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@openai.com",
                "add",
                ".",
            ])
            .current_dir(&workflow_dir)
            .status()
            .expect("git add");
        assert!(status.success(), "git add should succeed");
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Codex",
                "-c",
                "user.email=codex@openai.com",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(&workflow_dir)
            .status()
            .expect("git commit");
        assert!(status.success(), "git commit should succeed");

        workflow_dir
    }

    #[test]
    fn clean_workflow_does_not_block() {
        let home = TempDir::new().expect("create temp dir");
        let cwd = TempDir::new().expect("create temp dir");
        let workflow_dir = write_workflow_fixture(home.path(), "review/fix");

        let block_reason =
            workflow_quality_block_reason(home.path(), cwd.path(), &WorkflowsConfigToml::default())
                .expect("quality hook should run");

        assert_eq!(block_reason, None);
        assert!(workflow_dir.join("workflow.yaml").is_file());
    }

    #[test]
    fn dirty_valid_workflow_does_not_block() {
        let home = TempDir::new().expect("create temp dir");
        let cwd = TempDir::new().expect("create temp dir");
        let workflow_dir = write_workflow_fixture(home.path(), "review/fix");
        let workflow_source =
            fs::read_to_string(workflow_dir.join("src/workflow.ts")).expect("read workflow source");
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            format!("// extra comment to keep the workflow valid\n{workflow_source}"),
        )
        .expect("dirty valid workflow");

        let block_reason =
            workflow_quality_block_reason(home.path(), cwd.path(), &WorkflowsConfigToml::default())
                .expect("quality hook should run");

        assert_eq!(block_reason, None);
    }

    #[test]
    fn dirty_invalid_workflow_blocks() {
        let home = TempDir::new().expect("create temp dir");
        let cwd = TempDir::new().expect("create temp dir");
        let workflow_dir = write_workflow_fixture(home.path(), "review/fix");
        fs::write(
            workflow_dir.join("workflow.yaml"),
            "id: review/other\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    recovery: false\n",
        )
        .expect("make workflow invalid");

        let block_reason =
            workflow_quality_block_reason(home.path(), cwd.path(), &WorkflowsConfigToml::default())
                .expect("quality hook should run");

        assert!(block_reason.is_some());
        assert!(block_reason.as_deref().is_some_and(|reason| {
            reason.contains(
                "workflow.yaml id 'review/other' does not match directory id 'review/fix'",
            )
        }));
    }

    #[test]
    fn dirty_invalid_workflow_returns_reviewer_style_findings() {
        let home = TempDir::new().expect("create temp dir");
        let cwd = TempDir::new().expect("create temp dir");
        let workflow_dir = write_workflow_fixture(home.path(), "review/fix");
        fs::write(
            workflow_dir.join("workflow.yaml"),
            "id: review/other\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    recovery: false\n",
        )
        .expect("make workflow invalid");

        let feedback =
            workflow_quality_feedback(home.path(), cwd.path(), &WorkflowsConfigToml::default())
                .expect("quality hook should run")
                .expect("dirty invalid workflow should return feedback");

        assert!(feedback.reason.contains("Workflow quality findings:"));
        assert!(feedback.reason.contains("[WF-007]"));
        assert!(
            feedback.reason.contains(
                "workflow.yaml id 'review/other' does not match directory id 'review/fix'"
            )
        );
        assert!(
            feedback
                .additional_context
                .contains("Fix these findings in the current workflow development cycle")
        );
        assert!(feedback.additional_context.contains("WF-015"));
    }
}
