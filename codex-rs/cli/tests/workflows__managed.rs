use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::json;
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

#[test]
fn managed_discovery_shows_release_and_refuses_direct_edits() -> Result<()> {
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
            "automatic",
        ])
        .assert()
        .success();
    let commit = String::from_utf8(
        Command::new("git")
            .current_dir(&source)
            .args(["rev-parse", "HEAD"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned();
    let source_url = url::Url::from_file_path(source.canonicalize()?)
        .map_err(|_| anyhow::anyhow!("source URL"))?
        .to_string();
    let expected = json!({
        "source": source_url,
        "installed": {"tag": "v1.0.0", "version": "1.0.0", "commit": commit},
        "policy": "automatic",
        "dismissedRelease": null,
    });
    let listing = codex(home.path(), project.path())?
        .args(["workflow", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let listing: serde_json::Value = serde_json::from_slice(&listing)?;
    assert_eq!(listing[0]["managed"], expected);
    let shown = codex(home.path(), project.path())?
        .args(["workflow", "show", "team/build", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let shown: serde_json::Value = serde_json::from_slice(&shown)?;
    assert_eq!(shown["workflow"]["managed"], expected);
    codex(home.path(), project.path())?
        .args(["workflow", "status", "team/build"])
        .assert()
        .success()
        .stdout(contains("managed v1.0.0; policy automatic"));

    let active = home.path().join("workflows/team/build/workflow.yaml");
    let before = fs::read_to_string(&active)?;
    for args in [
        vec!["describe", "team/build", "changed"],
        vec!["docs", "team/build", "changed"],
        vec!["edit", "team/build", "changed"],
        vec!["repair", "team/build"],
        vec!["fix", "team/build"],
    ] {
        codex(home.path(), project.path())?
            .arg("workflow")
            .args(args)
            .assert()
            .failure()
            .stderr(contains("is a managed workflow"));
    }
    assert_eq!(fs::read_to_string(&active)?, before);
    assert!(!active.with_file_name("README.md").exists());

    let sibling = home.path().join("workflows/zz-override");
    fs::create_dir(&sibling)?;
    fs::write(sibling.join("workflow.yaml"), &before)?;
    let sibling_list = codex(home.path(), project.path())?
        .args(["workflow", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let sibling_list: serde_json::Value = serde_json::from_slice(&sibling_list)?;
    assert!(sibling_list[0].get("managed").is_none());
    codex(home.path(), project.path())?
        .args(["workflow", "describe", "team/build", "Sibling edit"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&active)?, before);
    fs::remove_dir_all(sibling)?;

    let project_workflow = project.path().join(".codex/workflows/team/build");
    fs::create_dir_all(&project_workflow)?;
    fs::write(project_workflow.join("workflow.yaml"), &before)?;
    let project_list = codex(home.path(), project.path())?
        .args(["workflow", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let project_list: serde_json::Value = serde_json::from_slice(&project_list)?;
    assert!(project_list[0].get("managed").is_none());
    let project_show = codex(home.path(), project.path())?
        .args(["workflow", "show", "team/build", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let project_show: serde_json::Value = serde_json::from_slice(&project_show)?;
    assert!(project_show["workflow"].get("managed").is_none());
    let project_status = codex(home.path(), project.path())?
        .args(["workflow", "status", "team/build"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&project_status).contains("managed"));
    codex(home.path(), project.path())?
        .args(["workflow", "describe", "team/build", "Project edit"])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(&active)?, before);
    fs::remove_dir_all(project_workflow)?;

    let changed_id = before.replace("id: team/build", "id: team/other");
    fs::write(&active, &changed_id)?;
    codex(home.path(), project.path())?
        .args(["workflow", "describe", "team/other", "Forbidden edit"])
        .assert()
        .failure()
        .stderr(contains("is a managed workflow"));
    assert_eq!(fs::read_to_string(&active)?, changed_id);
    Ok(())
}

#[cfg(unix)]
#[test]
fn developer_views_work_with_symlinked_root_and_read_only_home() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;

    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let workflow = project.path().join("developer-workflows/review");
    fs::create_dir_all(&workflow)?;
    fs::write(
        workflow.join("workflow.yaml"),
        "id: review\ncommand: review\nuserDescription: Developer workflow\n",
    )?;
    symlink(
        workflow.parent().context("developer root")?,
        home.path().join("workflows"),
    )?;
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let original = fs::metadata(home.path())?.permissions();
    fs::set_permissions(home.path(), fs::Permissions::from_mode(/*mode*/ 0o500))?;
    let listing = codex(home.path(), project.path())?
        .args(["workflow", "list", "--json"])
        .output()?;
    fs::set_permissions(home.path(), original)?;
    anyhow::ensure!(
        listing.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let listing: serde_json::Value = serde_json::from_slice(&listing.stdout)?;
    assert_eq!(listing[0]["id"], "review");
    Ok(())
}
