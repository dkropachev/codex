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

fn write_status_workflow(workflow_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let node_path = test_node_path()?;
    std::fs::create_dir_all(workflow_dir.join("src"))?;
    std::fs::create_dir_all(workflow_dir.join("state"))?;
    std::fs::create_dir_all(workflow_dir.join("node_modules/.bin"))?;
    std::fs::create_dir_all(workflow_dir.join(".git"))?;
    std::fs::write(workflow_dir.join("README.md"), "# Code Review\n")?;
    std::fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: code-review\ncommand: code-review\ntitle: Code Review\nuserDescription: Emit CLI workflow progress.\n",
    )?;
    std::fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "workflow-progress-test",
  "private": true,
  "type": "module"
}
"#,
    )?;
    std::fs::write(
        workflow_dir.join("src/workflow.ts"),
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
    )?;
    std::fs::write(
        workflow_dir.join("node_modules/.bin/tsx"),
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
fs.copyFileSync(workflowPath, tmpPath);
args[workflowPathIndex + 1] = tmpPath;
const result = spawnSync(process.execPath, [runner, ...args], {{ stdio: 'inherit' }});
process.exit(result.status ?? 1);
"#,
            node_path = node_path.display()
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(
        workflow_dir.join("node_modules/.bin/tsx"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    Ok(())
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
