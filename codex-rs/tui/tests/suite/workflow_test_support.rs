use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

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
        "#!/usr/bin/node\nconst fs = require('node:fs');\nconst os = require('node:os');\nconst path = require('node:path');\nconst { spawnSync } = require('node:child_process');\n\nconst [runner, ...args] = process.argv.slice(2);\nconst workflowPathIndex = args.indexOf('--workflow-path');\nif (workflowPathIndex === -1 || workflowPathIndex + 1 >= args.length) {\n  console.error('missing --workflow-path');\n  process.exit(1);\n}\nconst workflowPath = args[workflowPathIndex + 1];\nconst tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'workflow-runtime-'));\nconst tmpPath = path.join(tmpDir, path.basename(workflowPath) + '.mjs');\nfs.copyFileSync(workflowPath, tmpPath);\nargs[workflowPathIndex + 1] = tmpPath;\nconst result = spawnSync('/usr/bin/node', [runner, ...args], { stdio: 'inherit' });\nprocess.exit(result.status ?? 1);\n",
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(
        workflow_dir.join("node_modules/.bin/tsx"),
        std::fs::Permissions::from_mode(0o755),
    )?;
    Ok(())
}

pub(super) fn ensure_codex_binary(repo_root: &Path) -> Result<PathBuf> {
    let build_status = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg("codex-cli")
        .arg("--bin")
        .arg("codex")
        .current_dir(repo_root.join("codex-rs"))
        .status()?;
    anyhow::ensure!(build_status.success(), "failed to build codex binary");

    codex_utils_cargo_bin::cargo_bin("codex").map_err(Into::into)
}
