use super::repair_workflow_command;
use super::repair_workflow_command_with_runners;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;

use crate::execute::WorkflowCommandContext;
use crate::execute::WorkflowCommandOutput;
use crate::registry::WorkflowSummary;
use crate::repair::types::WorkflowRepairAction;
use crate::repair::types::WorkflowRepairActionKind;
use crate::spec::write_workflow_spec;
use crate::validation_runner::WorkflowValidationCommandResult;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

mod schema_and_docs;

fn write_runtime_gitignore(workflow_dir: &Path) {
    fs::write(
        workflow_dir.join(".gitignore"),
        "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
    )
    .unwrap();
}

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
    write_runtime_gitignore(workflow_dir);
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
        "export interface WorkflowInput { input?: string; }\nexport interface WorkflowOutput { ok: boolean; }\nexport const WorkflowOutput = { toTuiMarkdown() { return { markdown: \"done\" }; } };\nexport default async function workflow(_ctx: unknown, _input: WorkflowInput): Promise<WorkflowOutput> { return { ok: true }; }\nexport async function complete() { return []; }\n",
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
                "commands": ["node -e \"console.log('out'); console.error('err'); process.exit(1)\""],
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
    write_runtime_gitignore(workflow_dir);
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

fn write_layout_fixable_workflow_fixture(workflow_dir: &Path) {
    fs::create_dir_all(workflow_dir.join("src")).unwrap();
    fs::create_dir_all(workflow_dir.join(".git")).unwrap();
    fs::write(workflow_dir.join(".git/HEAD"), "ref: refs/heads/master\n").unwrap();
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
  "name": "codex-workflow-layout-fixable",
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
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &crate::spec::WorkflowSpec {
            id: "broken/layout".to_string(),
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

fn write_tracked_runtime_state_workflow_fixture(workflow_dir: &Path) {
    fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
    fs::create_dir_all(workflow_dir.join("state")).unwrap();
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
  "name": "codex-workflow-tracked-runtime-state",
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
    fs::write(workflow_dir.join("state/reviews.sqlite3"), "db").unwrap();
    write_workflow_spec(
        &workflow_dir.join("workflow.yaml"),
        &crate::spec::WorkflowSpec {
            id: "broken/runtime-state".to_string(),
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

    let status = Command::new("git")
        .args(["init"])
        .current_dir(workflow_dir)
        .status()
        .unwrap();
    assert!(status.success(), "git init should succeed");
    let status = Command::new("git")
        .args(["add", "."])
        .current_dir(workflow_dir)
        .status()
        .unwrap();
    assert!(status.success(), "git add should succeed");
}

#[cfg(unix)]
fn write_ai_repair_exec_script(exe_path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(
        exe_path,
        "#!/bin/sh\nif [ \"$1\" = \"exec\" ] && [ \"${2-}\" = \"--help\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"exec\" ]; then\n  count_file=state/ai-repair-count\n  count=0\n  if [ -f \"$count_file\" ]; then\n    count=$(cat \"$count_file\")\n  fi\n  count=$((count + 1))\n  mkdir -p state\n  printf '%s\\n' \"$count\" > \"$count_file\"\n  if [ \"$count\" -eq 1 ]; then\n    exit 0\n  fi\n  awk '{gsub(/process[.]exit[(]1[)]/, \"process.exit(0)\"); print}' workflow.yaml > workflow.yaml.tmp && mv workflow.yaml.tmp workflow.yaml\n  exit 0\nfi\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(exe_path, fs::Permissions::from_mode(0o755)).unwrap();
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
    let progress_events = Arc::new(Mutex::new(Vec::new()));
    let progress_events_for_callback = Arc::clone(&progress_events);
    let progress = move |event: crate::execute::WorkflowCommandProgress| {
        progress_events_for_callback
            .lock()
            .unwrap()
            .push(event.message);
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: None,
        stage_session_id: None,
        progress: Some(&progress),
    };

    let output: WorkflowCommandOutput = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert!(output.message.contains("Repairing workflow"));
    assert!(output.message.contains("Applied fixes:"));
    assert!(output.message.contains("Validation passed."));
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
    let progress_events = progress_events.lock().unwrap();
    assert!(
        progress_events
            .iter()
            .any(|message| message == "Resolving workflow")
    );
    assert!(
        progress_events
            .iter()
            .any(|message| message == "Validating workflow")
    );
    assert!(
        progress_events
            .iter()
            .any(|message| message == "Repair cycle started")
    );
    assert!(
        progress_events
            .iter()
            .any(|message| message == "Applied deterministic fixes")
    );
    assert!(
        progress_events
            .iter()
            .any(|message| message == "Workflow repair complete")
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
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert!(output.message.contains("Blocked findings:"));
    assert!(
        output
            .message
            .contains("repair mode `metadata` blocked the remaining findings")
    );
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
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert!(output.message.contains("Unsupported findings:"));
    assert_eq!(output.data["repair"]["stopReason"], "unsupportedFindings");
    assert!(
        output.data["repair"]["unsupportedFindings"]
            .as_array()
            .is_some_and(|findings| !findings.is_empty())
    );
    assert_eq!(
        output.data["validationCommandResults"][0]["command"],
        "node -e \"console.log('out'); console.error('err'); process.exit(1)\""
    );
    assert_eq!(
        output.data["validationCommandResults"][0]["succeeded"],
        false
    );
    assert!(
        output.data["validationCommandResults"][0]["stdout"]
            .as_str()
            .is_some_and(|stdout| stdout.contains("out"))
    );
    assert!(
        output.data["validationCommandResults"][0]["stderr"]
            .as_str()
            .is_some_and(|stderr| stderr.contains("err"))
    );
    assert_eq!(output.data["repair"]["changed"], false);
}

#[cfg(unix)]
#[test]
fn repair_workflow_command_uses_ai_fallback_until_validation_passes() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_command_failure_workflow_fixture(&workflow_dir);

    let codex_self_exe = home.path().join("fake-codex");
    write_ai_repair_exec_script(&codex_self_exe);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: Some(codex_self_exe),
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert!(output.message.contains("Validation passed."));
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["repair"]["changed"], true);
    assert_eq!(output.data["validation"]["findings"], serde_json::json!([]));
    assert_eq!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|fix| fix["kind"] == "aiRepair")
            .count(),
        2
    );
    assert_eq!(
        fs::read_to_string(workflow_dir.join("state/ai-repair-count"))
            .unwrap()
            .trim(),
        "2"
    );
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
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert!(output.message.contains("Validation passed."));
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert!(workflow_dir.join("tsconfig.json").is_file());
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| fixes.iter().any(|fix| fix["kind"] == "repairTsconfig"))
    );
}

#[test]
fn repair_workflow_command_refreshes_dependencies_for_broken_local_tsc() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/fix");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_build_fixable_workflow_fixture(&workflow_dir);
    fs::remove_dir_all(workflow_dir.join("node_modules")).unwrap();

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };
    let command_runner =
        |command: &str, cwd: &Path| -> anyhow::Result<WorkflowValidationCommandResult> {
            let installed_tsc = cwd.join("node_modules/typescript/lib/tsc.js").is_file();
            let succeeded = command != "npm run build" || installed_tsc;
            Ok(WorkflowValidationCommandResult {
                command: command.to_string(),
                succeeded,
                exit_code: Some(if succeeded { 0 } else { 1 }),
                stdout: String::new(),
                stderr: if succeeded {
                    String::new()
                } else {
                    "Error: Cannot find module '../lib/tsc.js'".to_string()
                },
            })
        };
    let mut install_calls = 0;
    let dependency_installer = |workflow: &WorkflowSummary, policy: &str| {
        assert_eq!(policy, "locked");
        install_calls += 1;
        fs::create_dir_all(workflow.path.join("node_modules/typescript/lib")).unwrap();
        fs::write(
            workflow.path.join("node_modules/typescript/lib/tsc.js"),
            "module.exports = {};",
        )
        .unwrap();
        Ok::<Option<WorkflowRepairAction>, anyhow::Error>(Some(WorkflowRepairAction {
            kind: WorkflowRepairActionKind::RepairPackageManifest,
            path: workflow.path.join("package-lock.json"),
            detail: "Installed workflow dependencies".to_string(),
        }))
    };

    let output = repair_workflow_command_with_runners(
        ctx,
        "broken/fix",
        command_runner,
        dependency_installer,
    )
    .unwrap();

    assert_eq!(install_calls, 1);
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["validation"]["findings"], serde_json::json!([]));
    assert!(
        output.data["repair"]["appliedFixes"]
            .as_array()
            .is_some_and(|fixes| fixes
                .iter()
                .any(|fix| fix["detail"] == "Installed workflow dependencies"))
    );
}

#[test]
fn repair_workflow_command_reports_created_layout_directories() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/layout");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_layout_fixable_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/layout").unwrap();

    assert!(output.message.contains("Created src/tests/ directory"));
    assert!(output.message.contains("Created state/ directory"));
    assert!(
        output
            .message
            .contains("Created state/.gitkeep placeholder")
    );
    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["repair"]["changed"], true);
    assert!(workflow_dir.join("src/tests").is_dir());
    assert!(workflow_dir.join("state").is_dir());
    assert!(workflow_dir.join("state/.gitkeep").is_file());
}

#[test]
fn repair_workflow_command_untracks_runtime_state_and_updates_gitignore() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let workflow_dir = home.path().join("workflows/broken/runtime-state");
    fs::create_dir_all(&workflow_dir).unwrap();
    write_tracked_runtime_state_workflow_fixture(&workflow_dir);

    let config = codex_config::types::WorkflowsConfigToml {
        commit_policy: Some("manual".to_string()),
        ..Default::default()
    };
    let ctx = WorkflowCommandContext {
        codex_home: home.path(),
        cwd: cwd.path(),
        config: &config,
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/runtime-state").unwrap();

    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert_eq!(output.data["repair"]["changed"], true);
    let gitignore = fs::read_to_string(workflow_dir.join(".gitignore")).unwrap();
    assert!(gitignore.contains("artifacts/"));
    assert!(gitignore.contains("state/*"));
    assert!(gitignore.contains("!state/.gitkeep"));
    assert!(workflow_dir.join("state/reviews.sqlite3").is_file());

    let tracked = Command::new("git")
        .args(["ls-files", "--", "state/reviews.sqlite3"])
        .current_dir(&workflow_dir)
        .output()
        .unwrap();
    assert!(tracked.status.success());
    assert_eq!(String::from_utf8(tracked.stdout).unwrap(), "");
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
        codex_self_exe: None,
        stage_session_id: None,
        progress: None,
    };

    let output = repair_workflow_command(ctx, "broken/fix").unwrap();

    assert_eq!(output.data["repair"]["stopReason"], "valid");
    assert!(output.message.contains("Validation passed."));

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
