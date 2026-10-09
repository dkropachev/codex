use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

const MANIFEST: &str = "apiVersion: 1\nid: team/build\ntitle: Team Build\ncallableName: team-build\ndescription: Build workflow\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn release(root: &Path, version: &str) -> Result<()> {
    fs::write(
        root.join("package.json"),
        format!(r#"{{"version":"{version}"}}"#),
    )?;
    fs::write(
        root.join("src/workflow.ts"),
        format!("export default '{version}';\n"),
    )?;
    git(root, &["add", "--all"])?;
    git(
        root,
        &["-c", "commit.gpgsign=false", "commit", "-qm", version],
    )?;
    git(root, &["tag", &format!("v{version}")])?;
    Ok(())
}

fn create_source(root: &Path, id: &str) -> Result<()> {
    fs::create_dir(root)?;
    fs::create_dir(root.join("src"))?;
    fs::write(
        root.join("workflow.yaml"),
        MANIFEST.replace("team/build", id),
    )?;
    git(root, &["init", "-q"])?;
    release(root, "1.0.0")
}

fn codex(home: &Path, cwd: &Path) -> Result<assert_cmd::Command> {
    let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    command
        .env("CODEX_HOME", home)
        .env("HOME", home)
        .current_dir(cwd);
    Ok(command)
}

#[test]
fn managed_cli_install_check_update_policy_dismiss_and_uninstall() -> Result<()> {
    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let source = project.path().join("source");
    create_source(&source, "team/build")?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;

    codex(home.path(), project.path())?
        .args([
            "workflow",
            "install",
            source.to_str().context("source path is UTF-8")?,
            "--policy",
            "manual",
        ])
        .assert()
        .success()
        .stdout(contains("Installed team/build"));
    codex(home.path(), project.path())?
        .args(["workflow", "check-updates", "team/build"])
        .assert()
        .success()
        .stdout(contains("team/build: current"));

    release(&source, "1.1.0")?;
    codex(home.path(), project.path())?
        .args(["workflow", "check-updates", "team/build"])
        .assert()
        .success()
        .stdout(contains("v1.1.0 available"));
    codex(home.path(), project.path())?
        .args(["workflow", "set-policy", "team/build", "automatic"])
        .assert()
        .success()
        .stdout(contains("policy set to automatic"));
    codex(home.path(), project.path())?
        .args(["workflow", "dismiss", "team/build"])
        .assert()
        .success()
        .stdout(contains("Dismissed v1.1.0"));
    codex(home.path(), project.path())?
        .args(["workflow", "check-updates", "team/build"])
        .assert()
        .success()
        .stdout(contains("v1.1.0 available (dismissed)"));
    codex(home.path(), project.path())?
        .args(["workflow", "update", "team/build"])
        .assert()
        .success()
        .stdout(contains("Updated team/build"));

    release(&source, "1.2.0")?;
    codex(home.path(), project.path())?
        .args(["workflow", "update", "--all"])
        .assert()
        .success()
        .stdout(contains("Updated team/build"));
    codex(home.path(), project.path())?
        .args(["workflow", "uninstall", "team/build"])
        .assert()
        .success()
        .stdout(contains("Uninstalled team/build"));
    assert!(!home.path().join("workflows/team/build").exists());
    Ok(())
}

#[test]
fn update_all_continues_after_one_workflow_fails() -> Result<()> {
    enum Failure {
        SourceUnavailable,
        DirtyPayload,
    }
    for failure in [Failure::SourceUnavailable, Failure::DirtyPayload] {
        let home = TempDir::new()?;
        let project = TempDir::new()?;
        let first = project.path().join("first");
        let second = project.path().join("second");
        create_source(&first, "team/build")?;
        create_source(&second, "team/other")?;
        fs::write(
            home.path().join("config.toml"),
            "[features]\nworkflows = true\n",
        )?;
        for path in [&first, &second] {
            codex(home.path(), project.path())?
                .args([
                    "workflow",
                    "install",
                    path.to_str().context("source path is UTF-8")?,
                ])
                .assert()
                .success();
        }
        let expected_error = match failure {
            Failure::SourceUnavailable => {
                fs::rename(&first, project.path().join("moved-first"))?;
                codex(home.path(), project.path())?
                    .args(["workflow", "dismiss", "team/build"])
                    .assert()
                    .failure()
                    .stderr(contains("source is unavailable"));
                "source is unavailable"
            }
            Failure::DirtyPayload => {
                release(&first, "1.1.0")?;
                fs::write(
                    home.path().join("workflows/team/build/src/workflow.ts"),
                    "modified locally\n",
                )?;
                "active payload differs"
            }
        };
        release(&second, "1.1.0")?;
        let result = codex(home.path(), project.path())?
            .args(["workflow", "update", "--all"])
            .assert()
            .failure()
            .stdout(contains("Updated team/other"));
        let stderr = String::from_utf8_lossy(&result.get_output().stderr);
        assert!(stderr.contains("team/build:"), "{stderr}");
        assert!(stderr.contains(expected_error), "{stderr}");
        assert!(
            stderr.contains("1 managed workflow update(s) failed"),
            "{stderr}"
        );
        codex(home.path(), project.path())?
            .args(["workflow", "check-updates", "team/other"])
            .assert()
            .success()
            .stdout(contains("team/other: current"));
        let receipt = fs::read(
            home.path()
                .join(".workflow-management/receipts/team/build/receipt.json"),
        )?;
        let receipt: serde_json::Value = serde_json::from_slice(&receipt)?;
        assert_eq!(receipt["installed"]["version"], "1.0.0");
    }
    Ok(())
}

#[test]
fn reserved_alias_can_be_run_explicitly() -> Result<()> {
    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let workflow = project.path().join(".codex/workflows/install");
    fs::create_dir_all(&workflow)?;
    fs::write(
        workflow.join("workflow.yaml"),
        "id: install\ncommand: install\nuserDescription: Existing alias\n",
    )?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    codex(home.path(), project.path())?
        .args(["workflow", "install"])
        .assert()
        .failure()
        .stderr(contains("install requires a Git source"));
    codex(home.path(), project.path())?
        .args(["workflow", "run", "install"])
        .assert()
        .failure()
        .stderr(contains("not a canonical package"));
    Ok(())
}
