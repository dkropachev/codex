use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn test_node_path() -> Result<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path_env| std::env::split_paths(&path_env).collect::<Vec<_>>())
        .flat_map(|dir| [dir.join("node"), dir.join("nodejs")])
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| anyhow::anyhow!("node executable should be available for workflow tests"))
}

fn write_config(codex_home: &Path, workspace: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace.join(".git"))?;
    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"model = "gpt-oss:20b"
model_provider = "ollama"
check_for_update_on_startup = false
suppress_unstable_features_warning = true

[analytics]
enabled = false

[projects."{workspace}"]
trust_level = "trusted"
"#,
            workspace = workspace.display()
        ),
    )?;
    Ok(())
}

fn write_workflow_fixture(
    workflow_dir: &Path,
    workflow_yaml: &str,
    workflow_source: &str,
) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let node_path = test_node_path()?;
    std::fs::create_dir_all(workflow_dir.join("src/tests"))?;
    std::fs::create_dir_all(workflow_dir.join("state"))?;
    std::fs::create_dir_all(workflow_dir.join("node_modules/.bin"))?;
    std::fs::create_dir_all(workflow_dir.join(".git"))?;
    std::fs::write(
        workflow_dir.join(".gitignore"),
        "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
    )?;
    std::fs::write(
        workflow_dir.join("README.md"),
        "# Workflow Fixture\n\n## Usage\n\nRun the fixture through a registered workflow command.\n\n## Workflow Runtime\n\nThe test fixture uses the process workflow runtime with a local Bun shim.\n\n## Dependencies\n\nThe fixture has no runtime package dependencies.\n\n## Validation\n\nStatic validation checks the package shape and coverage markers.\n\n## Maintenance\n\nKeep this fixture aligned with workflow validation requirements.\n",
    )?;
    std::fs::write(
        workflow_dir.join("DESIGN.md"),
        "# Workflow Fixture Design\n\n## Overview\n\nThis fixture exercises workflow command execution.\n\n## Architecture\n\nSource lives in src/ and tests live in src/tests/.\n\n## Data Flow\n\nThe CLI passes command input to the workflow runtime.\n\n## Failure Handling\n\nRuntime errors surface through the CLI command failure path.\n\n## Recovery Behavior\n\nNo recovery behavior is required for this fixture.\n\n## Test Matrix\n\nPositive, load, autocomplete, and negative markers keep discovery validation satisfied.\n\n## Maintenance Notes\n\nKeep package scripts and validation metadata in sync.\n",
    )?;
    std::fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "{workflow_yaml}\ndependencies:\n  runtime: []\n  development: []\nvalidation:\n  commands:\n    - bun build src/workflow.ts --target=bun --outdir artifacts/build\n    - bun test src/tests\n  contractSmoke:\n    input: {{}}\n  coverage:\n    positive: true\n    negative: true\n    progress: true\n    finalResult: true\n    failureUx: true\n    load: true\n    autocomplete: true\n    recovery: false\n"
        ),
    )?;
    std::fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "codex-workflow-progress-test",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "bun build src/workflow.ts --target=bun --outdir artifacts/build",
    "test": "bun test src/tests",
    "run": "bun src/workflow.ts"
  }
}
"#,
    )?;
    std::fs::write(
        workflow_dir.join("tsconfig.json"),
        "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"strict\": true,\n    \"noEmit\": true\n  },\n  \"include\": [\"src/**/*.ts\"]\n}\n",
    )?;
    std::fs::write(workflow_dir.join("src/workflow.ts"), workflow_source)?;
    std::fs::write(
        workflow_dir.join("src/tests/workflow.positive.test.ts"),
        "// workflow-covers: positive progress finalResult\nimport \"../workflow.ts\";\n",
    )?;
    std::fs::write(
        workflow_dir.join("src/tests/workflow.load.test.ts"),
        "// workflow-covers: load\nimport \"../workflow.ts\";\n",
    )?;
    std::fs::write(
        workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
        "// workflow-covers: autocomplete\nimport \"../workflow.ts\";\n",
    )?;
    std::fs::write(
        workflow_dir.join("src/tests/workflow.negative.test.ts"),
        "// workflow-covers: negative failureUx\nimport \"../workflow.ts\";\n",
    )?;
    std::fs::write(
        workflow_dir.join("node_modules/.bin/bun"),
        format!(
            r#"#!{node_path}
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {{ spawnSync }} = require('node:child_process');

const [runner, ...args] = process.argv.slice(2);
const workflowPathIndex = args.indexOf('--workflow-path');
if (workflowPathIndex === -1 || workflowPathIndex + 1 >= args.length) {{
  console.error('missing --workflow-path');
  process.exit(1);
}}
const workflowPath = args[workflowPathIndex + 1];
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'workflow-runtime-'));
const workflowDir = path.dirname(workflowPath);
const tmpWorkflowDir = path.join(tmpDir, path.basename(workflowDir));
fs.cpSync(workflowDir, tmpWorkflowDir, {{ recursive: true }});
const tmpPath = path.join(tmpWorkflowDir, path.basename(workflowPath) + '.mjs');
const source = fs.readFileSync(workflowPath, 'utf8')
  .replace(/export interface \w+ \{{[\s\S]*?\}}\n\n/g, '')
  .replace(/: unknown/g, '')
  .replace(/: WorkflowInput/g, '')
  .replace(/\): Promise<WorkflowOutput>/g, ')');
fs.writeFileSync(tmpPath, source);
args[workflowPathIndex + 1] = tmpPath;
const result = spawnSync(process.execPath, [runner, ...args], {{ stdio: 'inherit' }});
process.exit(result.status ?? 1);
"#,
            node_path = node_path.display()
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(
        workflow_dir.join("node_modules/.bin/bun"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    Ok(())
}

fn write_status_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        "id: code-review\ncommand: code-review\ntitle: Code Review\nuserDescription: Emit CLI workflow progress.\n",
        r#"const workflow = {
  async run(ctx) {
    ctx.status({
      workflowName: "code-review",
      workflowStatus: "initializing",
      threads: [{ name: "initializing", status: "normalizing input and resolving refs" }],
    });
    ctx.status({
      workflowName: "code-review",
      workflowStatus: "initial_review",
      threads: [{ name: "initial_review", status: "running analyzer for chunk-0001" }],
    });
    return { ok: true };
  },
};

export default workflow;
"#,
    )
}

fn write_input_workflow(workflow_dir: &Path) -> Result<()> {
    write_workflow_fixture(
        workflow_dir,
        r#"id: patch-impact
command: patch-impact
title: Patch Impact
userDescription: Echo workflow alias input.
"#,
        r#"export interface WorkflowInput {
  baseRef?: string;
  includeUntracked?: boolean;
  maxFiles?: number;
}

export interface WorkflowOutput {
  ok: boolean;
  input: WorkflowInput;
}

const workflow = {
  async run(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
    return { ok: true, input: { ...input } };
  },
};

export default workflow;
"#,
    )
}

#[test]
fn workflow_alias_progress_is_human_readable() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    write_config(codex_home.path(), workspace.path())?;
    write_status_workflow(&codex_home.path().join("workflows/code-review"))?;

    let output = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .env("CODEX_HOME", codex_home.path())
        .env("CODEX_WORKFLOW_RUNTIME_MODE", "process")
        .env_remove("CODEX_WORKFLOW_RUN_ID")
        .current_dir(workspace.path())
        .args([
            "--enable",
            "workflows",
            "-C",
            workspace.path().to_str().unwrap_or_default(),
            "-c",
            "analytics.enabled=false",
            "code-review",
        ])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;

    assert_eq!(stdout.trim(), "{\n  \"ok\": true\n}");
    assert!(stderr.contains("Workflow code-review: initializing"));
    assert!(stderr.contains("  -> initializing: normalizing input and resolving refs"));
    assert!(stderr.contains("Workflow code-review: initial_review"));
    assert!(stderr.contains("  -> initial_review: running analyzer for chunk-0001"));
    assert!(
        !stderr.contains("__CODEX_WORKFLOW_EVENT__"),
        "stderr should not contain raw workflow events: {stderr}"
    );
    assert!(
        !stderr.contains("\"workflowName\""),
        "stderr should not contain raw workflow JSON: {stderr}"
    );

    Ok(())
}

#[test]
fn workflow_alias_flags_are_mapped_to_json_input() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let codex_home = TempDir::new()?;
    let workspace = TempDir::new()?;
    write_config(codex_home.path(), workspace.path())?;
    write_input_workflow(&codex_home.path().join("workflows/patch-impact"))?;

    let run_alias = |args: &[&str]| -> Result<serde_json::Value> {
        let output = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
            .env("CODEX_HOME", codex_home.path())
            .env("CODEX_WORKFLOW_RUNTIME_MODE", "process")
            .env_remove("CODEX_WORKFLOW_RUN_ID")
            .current_dir(workspace.path())
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        Ok(serde_json::from_slice(&output.stdout)?)
    };

    let root_args = [
        "--enable",
        "workflows",
        "-C",
        workspace.path().to_str().unwrap_or_default(),
        "-c",
        "analytics.enabled=false",
        "patch-impact",
        "--base-ref",
        "HEAD",
        "--include-untracked",
        "--max-files",
        "20",
    ];
    let workflow_args = [
        "--enable",
        "workflows",
        "-C",
        workspace.path().to_str().unwrap_or_default(),
        "-c",
        "analytics.enabled=false",
        "workflow",
        "patch-impact",
        "--base-ref",
        "HEAD",
        "--include-untracked",
        "--max-files",
        "20",
    ];

    let expected = serde_json::json!({
        "ok": true,
        "input": {
            "baseRef": "HEAD",
            "includeUntracked": true,
            "maxFiles": 20,
        },
    });
    assert_eq!(run_alias(&root_args)?, expected);
    assert_eq!(run_alias(&workflow_args)?, expected);

    Ok(())
}
