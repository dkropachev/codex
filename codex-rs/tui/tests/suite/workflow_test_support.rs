use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

fn test_node_path() -> Result<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path_env| std::env::split_paths(&path_env).collect::<Vec<_>>())
        .flat_map(|dir| [dir.join("node"), dir.join("nodejs")])
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| anyhow::anyhow!("node executable should be available for workflow tests"))
}

pub(super) fn write_trusted_workspace_config(codex_home: &Path, workspace: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace.join(".git"))?;
    let config_contents = format!(
        r#"model = "gpt-oss:20b"
model_provider = "ollama"
check_for_update_on_startup = false
suppress_unstable_features_warning = true

[analytics]
enabled = false

[projects."{workspace}"]
trust_level = "trusted"
"#,
        workspace = workspace.display(),
    );
    std::fs::write(codex_home.join("config.toml"), config_contents)?;
    Ok(())
}

pub(super) fn write_workflow_fixture(
    workflow_dir: &Path,
    id: &str,
    command: &str,
    title: &str,
    workflow_source: &str,
) -> Result<()> {
    write_workflow_fixture_with_metadata(workflow_dir, id, command, title, workflow_source, "")
}

pub(super) fn write_workflow_fixture_with_metadata(
    workflow_dir: &Path,
    id: &str,
    command: &str,
    title: &str,
    workflow_source: &str,
    extra_workflow_yaml: &str,
) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let node_path = test_node_path()?;
    std::fs::create_dir_all(workflow_dir.join("src"))?;
    std::fs::create_dir_all(workflow_dir.join("state"))?;
    std::fs::create_dir_all(workflow_dir.join("node_modules/.bin"))?;
    std::fs::create_dir_all(workflow_dir.join(".git"))?;
    std::fs::write(workflow_dir.join("README.md"), format!("# {title}\n"))?;
    std::fs::write(workflow_dir.join("state/.gitkeep"), "")?;
    std::fs::write(
        workflow_dir.join("workflow.yaml"),
        format!(
            "id: {id}\ncommand: {command}\ntitle: {title}\nuserDescription: Emit progress and final markdown for TUI integration tests.\n{extra_workflow_yaml}"
        ),
    )?;
    std::fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "workflow-visibility-test",
  "private": true,
  "type": "module"
}
"#,
    )?;
    std::fs::write(workflow_dir.join("src/workflow.ts"), workflow_source)?;
    std::fs::write(
        workflow_dir.join("node_modules/.bin/tsx"),
        format!(
            r#"#!{}
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
            node_path.display(),
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(
        workflow_dir.join("node_modules/.bin/tsx"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    Ok(())
}

pub(super) fn ensure_codex_binary(repo_root: &Path) -> Result<PathBuf> {
    match Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("codex-cli")
        .arg("--bin")
        .arg("codex")
        .current_dir(repo_root.join("codex-rs"))
        .status()
    {
        Ok(build_status) => {
            anyhow::ensure!(build_status.success(), "failed to build codex binary");
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Bazel test environments do not provide cargo on PATH; use the runfile binary.
        }
        Err(err) => return Err(err.into()),
    }

    codex_utils_cargo_bin::cargo_bin("codex").map_err(Into::into)
}
