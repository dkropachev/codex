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

struct FakeBun {
    bin_dir: TempDir,
    capture_cwd: PathBuf,
    capture_args: PathBuf,
}

impl FakeBun {
    #[cfg(unix)]
    fn new(codex_home: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let bin_dir = TempDir::new()?;
        let capture_cwd = codex_home.join("captured-cwd.txt");
        let capture_args = codex_home.join("captured-args.txt");
        let fake_bun = bin_dir.path().join("bun");
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

        Ok(Self {
            bin_dir,
            capture_cwd,
            capture_args,
        })
    }

    fn apply_to_command(&self, cmd: &mut assert_cmd::Command) -> Result<()> {
        let old_path = std::env::var_os("PATH").context("PATH should be set")?;
        let path = std::env::join_paths(
            std::iter::once(self.bin_dir.path().to_path_buf())
                .chain(std::env::split_paths(&old_path)),
        )?;
        cmd.env("PATH", path)
            .env("CODEX_TEST_WORKFLOW_CWD", &self.capture_cwd)
            .env("CODEX_TEST_WORKFLOW_ARGS", &self.capture_args);
        Ok(())
    }

    fn captured_cwd(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.capture_cwd)?)
    }

    fn captured_args(&self) -> Result<Vec<String>> {
        Ok(fs::read_to_string(&self.capture_args)?
            .lines()
            .map(ToString::to_string)
            .collect())
    }
}

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
                "id": "code-review",
                "command": "code-review",
                "description": "Run a code review workflow.",
                "workflowDir": home_workflow_dir.display().to_string(),
            },
            {
                "id": "report",
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
            "Unknown workflow `missing`. Available workflows: code-review.",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_alias_invokes_bun_like_old_cli_surface() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "id: review/fix\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "code-review", "--scope", "repo"])
        .assert()
        .success();

    let args = fake_bun.captured_args()?;
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "argv": ["--scope", "repo"],
            "scope": "repo",
            "text": "--scope repo",
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_alias_positional_args_use_legacy_payload() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "id: review/fix\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "code-review", "current", "sprint"])
        .assert()
        .success();

    let args = fake_bun.captured_args()?;
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "argv": ["current", "sprint"],
            "text": "current sprint",
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_run_invokes_bun_with_structured_input() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args([
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

    let captured_cwd = fake_bun.captured_cwd()?;
    assert_eq!(captured_cwd.trim_end(), workflow_dir.display().to_string());

    let args = fake_bun.captured_args()?;
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "src/workflow.ts");
    assert_eq!(args[1], "--input");
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "action": "list-reports",
            "allowedAreas": "tui",
            "maxCount": 3,
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}

#[test]
fn workflow_management_commands_match_old_surface() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows").join("review"),
        "fix",
        "id: review/fix\ncommand: code-review\ntitle: /code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    fs::create_dir_all(workflow_dir.join("src"))?;
    fs::write(workflow_dir.join("src").join("workflow.ts"), "")?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow"])
        .assert()
        .success()
        .stdout(contains("Workflow Mode ready. 1 workflow(s) discovered."));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "list"])
        .assert()
        .success()
        .stdout(contains("review/fix"))
        .stdout(contains("/code-review"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "where", "review/fix"])
        .assert()
        .success()
        .stdout(contains(workflow_dir.display().to_string()));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "show", "review/fix"])
        .assert()
        .success()
        .stdout(contains("id: review/fix"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "validate", "review/fix"])
        .assert()
        .success()
        .stdout(contains("review/fix is valid"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "status", "review/fix"])
        .assert()
        .success()
        .stdout(contains("review/fix is clean"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "impact", "review/fix"])
        .assert()
        .success()
        .stdout(contains("\"id\": \"review/fix\""));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "config", "show"])
        .assert()
        .success()
        .stdout(contains("\"repairMode\": \"full\""));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "config", "set", "repair_mode", "threshold:2"])
        .assert()
        .success()
        .stdout(contains("Set workflows.repair_mode to threshold:2."));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "config", "clear", "repair_mode"])
        .assert()
        .success()
        .stdout(contains("Cleared workflows.repair_mode."));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "publish"])
        .assert()
        .failure()
        .stderr(contains("workflow publish requires a stage session id"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "--stage-session-id", "session", "publish"])
        .assert()
        .success()
        .stdout(contains("No staged workflow changes to publish."));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "--stage-session-id", "session", "discard"])
        .assert()
        .success()
        .stdout(contains("No staged workflow changes to discard."));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "--stage-session-id", "session", "done"])
        .assert()
        .success()
        .stdout(contains("Workflow Mode is done."));

    Ok(())
}

#[test]
fn workflow_editing_commands_match_old_surface() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "id: code-review\ncommand: code-review\nuserDescription: Old description\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args([
        "workflow",
        "describe",
        "code-review",
        "New workflow description",
    ])
    .assert()
    .success()
    .stdout(contains("Updated description for code-review"));
    let workflow_yaml = fs::read_to_string(workflow_dir.join("workflow.yaml"))?;
    assert!(workflow_yaml.contains("userDescription: \"New workflow description\""));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "docs", "code-review", "Document this behavior"])
        .assert()
        .success()
        .stdout(contains("Updated docs for code-review"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "edit", "code-review", "Change implementation"])
        .assert()
        .success()
        .stdout(contains("Updated docs for code-review"));
    let readme = fs::read_to_string(workflow_dir.join("README.md"))?;
    assert!(readme.contains("## Documentation"));
    assert!(readme.contains("Document this behavior"));
    assert!(readme.contains("## Edit request"));
    assert!(readme.contains("Change implementation"));

    Ok(())
}

#[test]
fn workflow_develop_scaffolds_project_workflow() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args([
        "workflow",
        "develop",
        "--location",
        "project",
        "--id",
        "reports/jira",
        "--command",
        "jira-report",
        "--title",
        "Jira Report",
        "Prepare Jira summaries",
    ])
    .assert()
    .success()
    .stdout(contains("Created workflow reports/jira"));

    let workflow_dir = project.path().join(".codex/workflows/reports/jira");
    let workflow_yaml = fs::read_to_string(workflow_dir.join("workflow.yaml"))?;
    assert!(workflow_yaml.contains("id: \"reports/jira\""));
    assert!(workflow_yaml.contains("command: \"jira-report\""));
    assert!(workflow_dir.join("src/workflow.ts").is_file());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_fix_invokes_bun_with_fix_action() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "fix", "code-review", "--scope", "repo"])
        .assert()
        .success();

    let args = fake_bun.captured_args()?;
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "action": "fix",
            "scope": "repo",
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_repair_alias_invokes_bun_with_fix_action() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "repair", "code-review"])
        .assert()
        .success();

    let args = fake_bun.captured_args()?;
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "action": "fix",
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_recover_invokes_bun_with_recover_action() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "recover", "code-review", "--failure-id", "abc"])
        .assert()
        .success();

    let args = fake_bun.captured_args()?;
    assert_eq!(
        serde_json::from_str::<Value>(&args[2])?,
        json!({
            "action": "recover",
            "failureId": "abc",
            "workingDirectory": project.path().display().to_string(),
        })
    );

    Ok(())
}
