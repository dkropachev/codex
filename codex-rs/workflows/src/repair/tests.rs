use super::repair_workflow_command;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::execute::WorkflowCommandContext;
use crate::execute::WorkflowCommandOutput;
use crate::spec::write_workflow_spec;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn write_broken_workflow_fixture(workflow_dir: &Path) {
    fs::write(
        workflow_dir.join("README.md"),
        "# Broken\n\n## Usage\n\n## Workflow Runtime\n",
    )
    .unwrap();
    fs::write(workflow_dir.join("DESIGN.md"), "# Broken Design\n").unwrap();
    fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "broken",
  "private": true,
  "type": "module"
}
"#,
    )
    .unwrap();
    fs::write(
        workflow_dir.join("workflow.ts"),
        r#"import leftPad from "left-pad";
import { WorkflowContext } from "@openai/codex-sdk/workflow";

export interface WorkflowInput { input?: string; }
export interface WorkflowOutput { ok: boolean; input: WorkflowInput; }
export const WorkflowOutput = { toTuiMarkdown() { return { markdown: "done" }; } };
export default async function run(_ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true, input: { input: leftPad(input.input ?? "", 2) } }; }
"#,
    )
    .unwrap();
    fs::write(
        workflow_dir.join("workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )
    .unwrap();
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &crate::spec::WorkflowSpec {
            id: "broken/other".to_string(),
            validation: json!({
                "commands": ["exit 0"],
                "coverage": {
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }
            }),
            ..Default::default()
        },
    )
    .unwrap();
}

fn write_command_failure_workflow_fixture(workflow_dir: &Path) {
    fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
    fs::create_dir_all(workflow_dir.join("state")).unwrap();
    fs::create_dir_all(workflow_dir.join(".git")).unwrap();
    fs::write(
        workflow_dir.join("README.md"),
        "# Workflow\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Workflow Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "codex-workflow-failing-command",
  "private": true,
  "type": "module"
}
"#,
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function workflow() { return { ok: true }; }\nexport async function complete() { return []; }\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )
    .unwrap();
    fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &crate::spec::WorkflowSpec {
            id: "broken/fix".to_string(),
            validation: json!({
                "commands": ["exit 1"],
                "coverage": {
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }
            }),
            ..Default::default()
        },
    )
    .unwrap();
}

fn write_build_fixable_workflow_fixture(workflow_dir: &Path) {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
    fs::create_dir_all(workflow_dir.join("state")).unwrap();
    fs::create_dir_all(workflow_dir.join(".git")).unwrap();
    fs::create_dir_all(workflow_dir.join("node_modules/.bin")).unwrap();
    fs::write(
        workflow_dir.join("README.md"),
        "# Workflow\n\n## Usage\n\n## Workflow Runtime\n\n## Dependencies\n\n## Validation\n\n## Maintenance\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Workflow Design\n\n## Overview\n\n## Architecture\n\n## Data Flow\n\n## Failure Handling\n\n## Recovery Behavior\n\n## Test Matrix\n\n## Maintenance Notes\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "codex-workflow-build-fixable",
  "private": true,
  "type": "module"
}
"#,
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/workflow.ts"),
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function workflow() { return { ok: true }; }\nexport async function complete() { return []; }\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nexport {};\n",
    )
    .unwrap();
    fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nexport {};\n",
    )
    .unwrap();
    fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
    fs::write(
        workflow_dir.join("node_modules/.bin/tsc"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    #[cfg(unix)]
    fs::set_permissions(
        workflow_dir.join("node_modules/.bin/tsc"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &crate::spec::WorkflowSpec {
            id: "broken/fix".to_string(),
            validation: json!({
                "commands": ["npm run build"],
                "coverage": {
                    "positive": true,
                    "negative": true,
                    "progress": true,
                    "finalResult": true,
                    "failureUx": true,
                    "load": true,
                    "autocomplete": true,
                    "recovery": false,
                }
            }),
            ..Default::default()
        },
    )
    .unwrap();
}

#[test]
fn repair_workflow_command_repairs_validation_findings_iteratively() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_broken_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        stage_session_id: None,
    };

    let output: WorkflowCommandOutput = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.message, "valid");
    assert_eq!(output.data["repair"]["changed"], true);
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["validation"]["findings"], serde_json::json!([]));
    assert!(workflow_dir.join("src/workflow.ts").is_file());
    assert!(
        workflow_dir
            .join("src/tests/workflow.positive.test.ts")
            .is_file()
    );
    assert!(workflow_dir.join("README.md").is_file());
    assert!(workflow_dir.join("DESIGN.md").is_file());
    let package = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(workflow_dir.join("package.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(package["dependencies"]["left-pad"], "latest");
    assert_eq!(package["dependencies"]["@openai/codex-sdk"], "latest");
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| !fixes.is_empty())
    );
}

#[test]
fn repair_workflow_command_reports_blocked_findings_when_mode_is_too_narrow() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_broken_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        repair_mode: Some("metadata".to_string()),
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        stage_session_id: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.data["repair"]["stopReason"], "blockedByRepairMode");
    assert!(
        output.data["repair"]["blockedFindings"]
            .as_array()
            .is_some_and(|findings| !findings.is_empty())
    );
    assert!(
        output.data["repair"]["unsupportedFindings"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty)
    );
}

#[test]
fn repair_workflow_command_reports_unsupported_validation_command_failures() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_command_failure_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        stage_session_id: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.data["repair"]["stopReason"], "unsupportedFindings");
    assert!(
        output.data["repair"]["unsupportedFindings"]
            .as_array()
            .is_some_and(|findings| !findings.is_empty())
    );
    assert_eq!(output.data["repair"]["changed"], false);
}

#[test]
fn repair_workflow_command_applies_known_build_command_fixers() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_build_fixable_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        stage_session_id: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.message, "valid");
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert!(workflow_dir.join("tsconfig.json").is_file());
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| fixes.iter().any(|fix| fix["kind"] == "repairTsconfig"))
    );
}

#[test]
fn repair_workflow_command_commits_successful_repairs_when_commit_policy_allows_it() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_broken_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml::default();
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        stage_session_id: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.data["repair"]["stopReason"], "valid");

    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workflow_dir)
        .output()
        .unwrap();
    assert!(head.status.success());
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&workflow_dir)
        .output()
        .unwrap();
    assert!(status.status.success());
    assert_eq!(String::from_utf8(status.stdout).unwrap().trim(), "");
}
