use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

fn codex_command(codex_home: &Path, cwd: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home)
        .env("HOME", codex_home)
        .current_dir(cwd);
    Ok(cmd)
}

fn enable_workflows(codex_home: &Path) -> Result<()> {
    fs::write(
        codex_home.join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    Ok(())
}

fn write_workflow(root: &Path, dirname: &str, yaml: &str) -> Result<PathBuf> {
    let workflow_dir = root.join(dirname);
    fs::create_dir_all(&workflow_dir)?;
    fs::write(workflow_dir.join("workflow.yaml"), yaml)?;
    Ok(workflow_dir)
}

#[test]
fn workflow_list_requires_workflows_feature() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "list"])
        .assert()
        .failure()
        .stderr(contains("requires the `workflows` feature"));

    Ok(())
}

#[test]
fn workflow_list_outputs_discovered_commands_as_json() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let home_workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        r#"
id: code-review
command: code-review
title: /code-review
userDescription: Run a code review workflow.
"#,
    )?;
    let project_workflow_dir = write_workflow(
        &project.path().join(".codex").join("workflows"),
        "report",
        r#"
id: report
command: report
userDescription: Build a project report.
"#,
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    let output = cmd
        .args(["workflow", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let actual = serde_json::from_slice::<Value>(&output)?;

    assert_eq!(
        actual,
        json!([
            {
                "command": "code-review",
                "description": "Run a code review workflow.",
                "workflowDir": home_workflow_dir.display().to_string(),
            },
            {
                "command": "report",
                "description": "Build a project report.",
                "workflowDir": project_workflow_dir.display().to_string(),
            },
        ])
    );

    Ok(())
}

#[test]
fn workflow_run_unknown_command_reports_available_commands() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "run", "missing"])
        .assert()
        .failure()
        .stderr(contains(
            "Unknown workflow command `missing`. Available commands: code-review.",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_run_invokes_bun_with_structured_input() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    let fake_bin = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let capture_cwd = codex_home.path().join("captured-cwd.txt");
    let capture_args = codex_home.path().join("captured-args.txt");
    let fake_bun = fake_bin.path().join("bun");
    fs::write(
        &fake_bun,
        r#"#!/bin/sh
printf '%s\n' "$PWD" > "$CODEX_TEST_WORKFLOW_CWD"
: > "$CODEX_TEST_WORKFLOW_ARGS"
for arg in "$@"; do
  printf '%s\n' "$arg" >> "$CODEX_TEST_WORKFLOW_ARGS"
done
"#,
    )?;
    let mut permissions = fs::metadata(&fake_bun)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_bun, permissions)?;
    let old_path = std::env::var_os("PATH").context("PATH should be set")?;
    let path = std::env::join_paths(
        std::iter::once(fake_bin.path().to_path_buf()).chain(std::env::split_paths(&old_path)),
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.env("PATH", path)
        .env("CODEX_TEST_WORKFLOW_CWD", &capture_cwd)
        .env("CODEX_TEST_WORKFLOW_ARGS", &capture_args)
        .args([
            "workflow",
            "run",
            "code-review",
            "--action",
            "list-reports",
            "--allowed-areas",
            "tui",
            "--max-count",
            "3",
        ])
        .assert()
        .success();

    let captured_cwd = fs::read_to_string(capture_cwd)?;
    assert_eq!(captured_cwd.trim_end(), workflow_dir.display().to_string());

    let captured_args = fs::read_to_string(capture_args)?;
    let args = captured_args.lines().collect::<Vec<_>>();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "src/workflow.ts");
    assert_eq!(args[1], "--input");
    assert_eq!(
        serde_json::from_str::<Value>(args[2])?,
        json!({
            "action": "list-reports",
            "allowedAreas": "tui",
            "maxCount": 3,
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}
