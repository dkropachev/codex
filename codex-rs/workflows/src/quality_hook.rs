use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use anyhow::anyhow;
use codex_config::types::WorkflowsConfigToml;

use crate::registry::WorkflowSummary;
use crate::registry::discover_workflows;
use crate::registry::workflow_git_status;
use crate::validation_runner::WorkflowValidationCommandResult;
use crate::validation_runner::WorkflowValidationReport;
use crate::validation_runner::run_validation_command;
use crate::validation_runner::validate_workflow;
use crate::validation_runner::validation_report_message;

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

    Ok(
        workflow_quality_failure_for_workflow(&workflow)?.map(|failure| {
            render_findings(
                std::slice::from_ref(&failure),
                /*include_guidance*/ false,
            )
        }),
    )
}

fn workflow_quality_failure_for_workflow(
    workflow: &WorkflowSummary,
) -> Result<Option<WorkflowQualityFailure>> {
    if workflow_git_status(workflow).is_empty() {
        return Ok(None);
    }

    let report = validate_workflow(workflow, run_validation_command)?;
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
    let failed_command_result = report
        .command_results
        .iter()
        .rev()
        .find(|result| !result.succeeded);
    let mut findings = report
        .messages
        .iter()
        .map(|message| finding_for_message(workflow, message, failed_command_result))
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

fn finding_for_message(
    workflow: &WorkflowSummary,
    message: &str,
    failed_command_result: Option<&WorkflowValidationCommandResult>,
) -> WorkflowQualityFinding {
    let (rule_id, title, path) = classify_finding(workflow, message);
    let body = if message.starts_with("validation command `") {
        render_command_failure_body(message, failed_command_result)
    } else {
        message.to_string()
    };
    WorkflowQualityFinding {
        rule_id,
        title,
        path,
        body,
    }
}

fn classify_finding(
    workflow: &WorkflowSummary,
    message: &str,
) -> (&'static str, &'static str, PathBuf) {
    if message.starts_with("missing README.md")
        || message.contains("README.md is missing required heading")
    {
        (
            "WF-001",
            "README.md is incomplete or missing",
            workflow.path.join("README.md"),
        )
    } else if message.starts_with("missing DESIGN.md")
        || message.contains("DESIGN.md is missing required heading")
    {
        (
            "WF-002",
            "DESIGN.md is incomplete or missing",
            workflow.path.join("DESIGN.md"),
        )
    } else if message.contains("imports undeclared package")
        || message.contains("package manifest")
        || message.starts_with("missing package.json")
    {
        (
            "WF-004",
            "Workflow dependencies are not self-contained",
            package_related_path(workflow, message),
        )
    } else if is_positive_coverage_finding(message) {
        (
            "WF-008",
            "Positive-path coverage is missing or inaccurate",
            coverage_related_path(workflow, message),
        )
    } else if is_negative_coverage_finding(message) {
        (
            "WF-009",
            "Negative and failure-path coverage is missing or inaccurate",
            coverage_related_path(workflow, message),
        )
    } else if is_recovery_coverage_finding(message) {
        (
            "WF-010",
            "Recovery coverage is missing or inaccurate",
            coverage_related_path(workflow, message),
        )
    } else if is_validation_contract_finding(message) {
        (
            "WF-007",
            "Workflow validation metadata or commands are inaccurate",
            validation_related_path(workflow, message),
        )
    } else if is_layout_finding(message) {
        (
            "WF-003",
            "Workflow layout is invalid",
            layout_related_path(workflow, message),
        )
    } else {
        (
            "WF-011",
            "Workflow validation surfaced a stability or correctness issue",
            workflow.path.clone(),
        )
    }
}

fn is_positive_coverage_finding(message: &str) -> bool {
    coverage_message_mentions(message, "positive")
        || coverage_message_mentions(message, "progress")
        || coverage_message_mentions(message, "finalResult")
}

fn is_negative_coverage_finding(message: &str) -> bool {
    coverage_message_mentions(message, "negative")
        || coverage_message_mentions(message, "failureUx")
}

fn is_recovery_coverage_finding(message: &str) -> bool {
    coverage_message_mentions(message, "recovery")
}

fn coverage_message_mentions(message: &str, key: &str) -> bool {
    message.contains(&format!("validation.coverage.{key}"))
        || message.contains(&format!("workflow-covers: {key}"))
}

fn is_validation_contract_finding(message: &str) -> bool {
    message.contains("workflow.yaml")
        || message.contains("validation.commands")
        || message.contains("validation.coverage")
        || message.starts_with("validation command `")
}

fn is_layout_finding(message: &str) -> bool {
    message.starts_with("missing src/")
        || message.starts_with("missing src/tests/")
        || message.starts_with("missing state/")
        || message.starts_with("missing src/workflow.ts")
        || message.contains("workflow directory is not a git repository")
        || message.contains("workflow path ")
        || message.contains("code files must live under src/")
        || message.contains("test files must live under src/tests/")
        || message.contains("database files must live under state/")
}

fn package_related_path(workflow: &WorkflowSummary, message: &str) -> PathBuf {
    if let Some(relative) =
        extract_relative_path(message, "source file ", " imports undeclared package")
    {
        workflow.path.join(relative)
    } else {
        workflow.path.join("package.json")
    }
}

fn coverage_related_path(workflow: &WorkflowSummary, message: &str) -> PathBuf {
    if message.contains("validation.coverage") {
        workflow.workflow_yaml_path.clone()
    } else {
        workflow.path.join("src/tests")
    }
}

fn validation_related_path(workflow: &WorkflowSummary, message: &str) -> PathBuf {
    if message.starts_with("validation command `") {
        workflow.path.clone()
    } else {
        workflow.workflow_yaml_path.clone()
    }
}

fn layout_related_path(workflow: &WorkflowSummary, message: &str) -> PathBuf {
    if message.contains("src/tests/") {
        workflow.path.join("src/tests")
    } else if message.contains("src/") {
        workflow.path.join("src")
    } else if message.contains("state/") {
        workflow.path.join("state")
    } else if message.contains("git repository") {
        workflow.path.join(".git")
    } else {
        workflow.path.clone()
    }
}

fn extract_relative_path<'a>(message: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let rest = message.strip_prefix(prefix)?;
    rest.strip_suffix(suffix)
}

fn render_command_failure_body(
    message: &str,
    failed_command_result: Option<&WorkflowValidationCommandResult>,
) -> String {
    let Some(result) = failed_command_result else {
        return message.to_string();
    };

    let mut lines = vec![
        message.to_string(),
        format!("Command: `{}`", result.command),
    ];
    let exit_label = result
        .exit_code
        .map(|code| format!("Exit status: {code}"))
        .unwrap_or_else(|| "Exit status: non-zero".to_string());
    lines.push(exit_label);

    if let Some(stderr) = trimmed_output_snippet(&result.stderr) {
        lines.push("stderr:".to_string());
        lines.extend(stderr.lines().map(ToString::to_string));
    } else if let Some(stdout) = trimmed_output_snippet(&result.stdout) {
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
            "Fix these findings in the current workflow development cycle before treating the workflow as complete."
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
            workflow_dir.join("README.md"),
            "# Test\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
        )
        .expect("write README");
        fs::write(
            workflow_dir.join("DESIGN.md"),
            "# Test Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
        )
        .expect("write DESIGN");
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "codex-workflow-test",
  "private": true,
  "type": "module"
}
"#,
        )
        .expect("write package.json");
        fs::write(workflow_dir.join("src/workflow.ts"), "export {}\n").expect("write workflow");
        fs::write(
            workflow_dir.join("src/tests/workflow.positive.test.ts"),
            "// workflow-covers: positive progress finalResult\nexport {}\n",
        )
        .expect("write positive test");
        fs::write(
            workflow_dir.join("src/tests/workflow.negative.test.ts"),
            "// workflow-covers: negative failureUx\nexport {}\n",
        )
        .expect("write negative test");
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nexport {}\n",
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
            "id: review/fix\nvalidation:\n  commands:\n    - exit 0\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n",
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
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            "// extra comment to keep the workflow valid\nexport {}\n",
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
