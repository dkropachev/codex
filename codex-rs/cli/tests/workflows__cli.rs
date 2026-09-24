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
    capture_input: PathBuf,
}

impl FakeBun {
    #[cfg(unix)]
    fn new(codex_home: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let bin_dir = TempDir::new()?;
        let capture_cwd = codex_home.join("captured-cwd.txt");
        let capture_input = codex_home.join("captured-input.json");
        let fake_bun = bin_dir.path().join("bun");
        fs::write(
            &fake_bun,
            r#"#!/bin/sh
printf '%s\n' "$PWD" > "$CODEX_TEST_WORKFLOW_CWD"
while [ "${1#--}" != "$1" ]; do
  shift
done
if [ "${2:-}" = "inspect" ]; then
  workflow_id=$(sed -n 's/^id: //p' workflow.yaml)
  workflow_title=$(sed -n 's/^title: //p' workflow.yaml)
  callable_name=$(sed -n 's/^callableName: //p' workflow.yaml)
  printf '{"apiVersion":1,"id":"%s","title":"%s","callableName":"%s","inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"workingDirectory":{"type":"string"}},"additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"hasComplete":true}\n' \
    "$workflow_id" "$workflow_title" "$callable_name"
fi
if [ "${2:-}" = "scan" ]; then
  printf '%s\n' '[{"path":"src/workflow.ts","exports":["inputSchema","outputSchema"],"imports":[]}]'
fi
if [ "${2:-}" = "run" ]; then
  cat "${3:?}" > "$CODEX_TEST_WORKFLOW_INPUT"
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":1,"method":"contract","params":{"inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":2,"method":"validateOutput","params":{"output":{}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":0,"method":"complete","params":{"markdown":"ok\n"}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
fi
"#,
        )?;
        let mut permissions = fs::metadata(&fake_bun)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_bun, permissions)?;

        Ok(Self {
            bin_dir,
            capture_cwd,
            capture_input,
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
            .env("CODEX_TEST_WORKFLOW_INPUT", &self.capture_input);
        Ok(())
    }

    fn captured_cwd(&self) -> Result<String> {
        Ok(fs::read_to_string(&self.capture_cwd)?)
    }

    fn captured_workflow_input(&self) -> Result<Value> {
        Ok(serde_json::from_str(&fs::read_to_string(
            &self.capture_input,
        )?)?)
    }

    fn was_invoked(&self) -> bool {
        self.capture_cwd.exists() || self.capture_input.exists()
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

fn existing_path_display(path: &Path) -> Result<String> {
    Ok(path.canonicalize()?.display().to_string())
}

fn write_workflow(root: &Path, dirname: &str, yaml: &str) -> Result<PathBuf> {
    let workflow_dir = root.join(dirname);
    fs::create_dir_all(&workflow_dir)?;
    fs::write(workflow_dir.join("workflow.yaml"), yaml)?;
    Ok(workflow_dir)
}

fn scaffold_canonical_workflow(
    codex_home: &Path,
    cwd: &Path,
    id: &str,
    callable_name: &str,
) -> Result<PathBuf> {
    let mut cmd = codex_command(codex_home, cwd)?;
    cmd.args([
        "workflow",
        "develop",
        "--location",
        "global",
        "--id",
        id,
        "--command",
        callable_name,
        "Test workflow",
    ])
    .assert()
    .success();
    Ok(codex_home.join("workflows").join(id))
}

fn write_workflow_source(workflow_dir: &Path) -> Result<()> {
    fs::create_dir_all(workflow_dir.join("src"))?;
    fs::write(workflow_dir.join("src").join("workflow.ts"), "")?;
    Ok(())
}

fn assert_code_review_static_metadata_without_legacy_usage(workflow_dir: &Path) -> Result<()> {
    let workflow_yaml = fs::read_to_string(workflow_dir.join("workflow.yaml"))?;
    assert!(
        workflow_yaml.contains("id: code-review") || workflow_yaml.contains("id: \"code-review\"")
    );
    assert!(
        workflow_yaml.contains("command: code-review")
            || workflow_yaml.contains("command: \"code-review\"")
    );
    assert!(workflow_yaml.contains("title:"));
    assert!(workflow_yaml.contains("userDescription:"));
    assert!(!workflow_yaml.contains("usage:\n  options:"));
    for legacy_fragment in [
        "read-report",
        "list-reports",
        "incremental",
        "--action",
        "--review-id",
        "--target-ref",
        "--base-ref",
        "--review-model",
        "--repro-model",
        "--include-skipped-by-limit",
    ] {
        assert!(!workflow_yaml.contains(legacy_fragment));
    }
    Ok(())
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
                "workflowDir": existing_path_display(&home_workflow_dir)?,
            },
            {
                "id": "report",
                "command": "report",
                "description": "Build a project report.",
                "workflowDir": existing_path_display(&project_workflow_dir)?,
            },
        ])
    );

    Ok(())
}

#[test]
fn exact_workflow_id_takes_precedence_over_another_packages_alias() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let root = codex_home.path().join("workflows");
    let alpha = write_workflow(
        &root,
        "alpha",
        "id: alpha\ncommand: beta\nuserDescription: Alias owner\n",
    )?;
    let beta = write_workflow(
        &root,
        "beta",
        "id: beta\ncommand: other\nuserDescription: Exact ID owner\n",
    )?;
    write_workflow(
        &root,
        "gamma",
        "id: gamma\ncommand: duplicate\nuserDescription: First duplicate\n",
    )?;
    write_workflow(
        &root,
        "delta",
        "id: delta\ncommand: duplicate\nuserDescription: Second duplicate\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "where", "beta"])
        .assert()
        .success()
        .stdout(format!("{}\n", existing_path_display(&beta)?));
    assert_ne!(alpha, beta);

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "duplicate"])
        .assert()
        .failure()
        .stderr(contains("alias `duplicate` is ambiguous"));

    Ok(())
}

#[test]
fn workflow_fix_prefers_an_exact_id_over_another_packages_alias() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let root = codex_home.path().join("workflows");
    let alias_owner = write_workflow(
        &root,
        "alpha",
        "id: alpha\ncommand: code-review\nuserDescription: Alias owner\n",
    )?;
    write_workflow_source(&alias_owner)?;
    let exact = write_workflow(
        &root,
        "code-review",
        "id: code-review\ncommand: other\nuserDescription: Exact ID owner\n",
    )?;
    write_workflow_source(&exact)?;
    let alias_metadata = fs::read_to_string(alias_owner.join("workflow.yaml"))?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "fix", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "Repairing workflow code-review with compatibility mode.",
        ));

    assert_eq!(
        fs::read_to_string(alias_owner.join("workflow.yaml"))?,
        alias_metadata
    );
    assert_code_review_static_metadata_without_legacy_usage(&exact)?;
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

#[test]
fn workflow_run_legacy_package_reports_migration_guidance() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "id: code-review\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "run", "code-review", "--scope", "repo"])
        .assert()
        .failure()
        .stderr(contains("is not a canonical package"))
        .stderr(contains("migrate to workflow package v1"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_run_rejects_an_incomplete_canonical_package() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = scaffold_canonical_workflow(
        codex_home.path(),
        project.path(),
        "incomplete",
        "incomplete",
    )?;
    fs::remove_file(workflow_dir.join("README.md"))?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "run", "incomplete"])
        .assert()
        .failure()
        .stderr(contains("does not satisfy the canonical v1 contract"))
        .stderr(contains("README.md"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_alias_invokes_bun_like_old_cli_surface() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    scaffold_canonical_workflow(
        codex_home.path(),
        project.path(),
        "review/fix",
        "code-review",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "code-review", "--scope", "repo"])
        .assert()
        .success();

    assert_eq!(
        fake_bun.captured_workflow_input()?,
        json!({
            "scope": "repo",
            "workingDirectory": existing_path_display(project.path())?,
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_alias_positional_args_report_migration_guidance() -> Result<()> {
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
        .failure()
        .stderr(contains(
            "positional value 'current' is no longer supported",
        ))
        .stderr(contains("--kebab-case flags"))
        .stderr(contains("--input"));
    assert!(!fake_bun.was_invoked());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_run_invokes_bun_with_structured_input() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = scaffold_canonical_workflow(
        codex_home.path(),
        project.path(),
        "code-review",
        "code-review",
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
    assert_eq!(
        captured_cwd.trim_end(),
        existing_path_display(&workflow_dir)?
    );

    assert_eq!(
        fake_bun.captured_workflow_input()?,
        json!({
            "action": "list-reports",
            "allowedAreas": "tui",
            "maxCount": 3,
            "workingDirectory": existing_path_display(project.path())?,
        })
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_run_by_nested_id_merges_json_input_and_flags() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = scaffold_canonical_workflow(
        codex_home.path(),
        project.path(),
        "review/fix",
        "code-review",
    )?;
    fs::write(
        project.path().join("input.json"),
        r#"{"scope":"repo","workingDirectory":"/tmp/custom"}"#,
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args([
        "workflow",
        "run",
        "review/fix",
        "--input",
        "@input.json",
        "--scope",
        "cli",
        "--max-count",
        "3",
    ])
    .assert()
    .success();

    let captured_cwd = fake_bun.captured_cwd()?;
    assert_eq!(
        captured_cwd.trim_end(),
        existing_path_display(&workflow_dir)?
    );

    assert_eq!(
        fake_bun.captured_workflow_input()?,
        json!({
            "scope": "cli",
            "maxCount": 3,
            "workingDirectory": "/tmp/custom",
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
        .stdout(contains(existing_path_display(&workflow_dir)?));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "show", "review/fix"])
        .assert()
        .success()
        .stdout(contains("id: review/fix"));

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "validate", "review/fix"])
        .assert()
        .failure()
        .stdout(contains("migrate"));

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
fn workflow_show_json_and_root_status_cover_management_outputs() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "review/fix",
        "id: review/fix\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    let output = cmd
        .args(["workflow", "show", "review/fix", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&output)?,
        json!({
            "workflow": {
                "id": "review/fix",
                "command": "code-review",
                "description": "Run a code review workflow.",
                "workflowDir": existing_path_display(&workflow_dir)?,
            },
            "workflowYaml": "id: review/fix\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
        })
    );

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "status"])
        .assert()
        .success()
        .stdout(contains("1 workflow(s) discovered"));

    Ok(())
}

#[test]
fn workflow_validate_reports_invalid_workflow_at_cli_boundary() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    write_workflow(
        &codex_home.path().join("workflows"),
        "review/fix",
        "id: review/fix\ncommand: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "validate", "review/fix"])
        .assert()
        .failure()
        .stdout(contains("missing"))
        .stdout(contains("src/workflow.ts"));

    Ok(())
}

#[test]
fn workflow_validate_reports_an_ambiguous_alias() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let root = codex_home.path().join("workflows");
    write_workflow(
        &root,
        "alpha",
        "id: alpha\ncommand: duplicate\nuserDescription: First duplicate\n",
    )?;
    write_workflow(
        &root,
        "beta",
        "id: beta\ncommand: duplicate\nuserDescription: Second duplicate\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "validate", "duplicate"])
        .assert()
        .failure()
        .stderr(contains("alias `duplicate` is ambiguous"));

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
    assert!(workflow_yaml.contains("apiVersion: 1"));
    assert!(workflow_yaml.contains("id: reports/jira"));
    assert!(workflow_yaml.contains("callableName: jira-report"));
    assert!(workflow_dir.join("package.json").is_file());
    assert!(workflow_dir.join(".gitignore").is_file());
    assert!(workflow_dir.join("README.md").is_file());
    assert!(workflow_dir.join("DESIGN.md").is_file());
    assert!(workflow_dir.join("src/workflow.ts").is_file());
    assert!(workflow_dir.join("src/tests").is_dir());
    assert!(workflow_dir.join("state/.gitkeep").is_file());
    assert!(workflow_dir.join(".git").is_dir());

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_validate_prints_exact_success_marker_for_fresh_scaffold() -> Result<()> {
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
        "hello",
        "A hello workflow",
    ])
    .assert()
    .success();

    let fake_bun = FakeBun::new(codex_home.path())?;
    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "validate", "hello"])
        .assert()
        .success()
        .stdout("valid\n");

    Ok(())
}

#[test]
fn workflow_validate_reports_an_undiscoverable_package_by_safe_id_path() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir =
        scaffold_canonical_workflow(codex_home.path(), project.path(), "broken", "broken")?;
    let manifest_path = workflow_dir.join("workflow.yaml");
    let manifest = fs::read_to_string(&manifest_path)?;
    fs::write(
        &manifest_path,
        manifest
            .lines()
            .filter(|line| !line.starts_with("callableName:"))
            .collect::<Vec<_>>()
            .join("\n"),
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "validate", "broken"])
        .assert()
        .failure()
        .stdout(contains("metadata:"))
        .stdout(contains("callableName"))
        .stderr(predicates::str::is_empty());

    Ok(())
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn workflow_run_executes_fresh_scaffold_and_formats_markdown() -> Result<()> {
    which::which("bun").context("workflow runtime tests require Bun")?;

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
        "hello",
        "A hello workflow",
    ])
    .assert()
    .success();

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "run", "hello", "--message", "Ready."])
        .assert()
        .success()
        .stdout("# Hello\n\nReady.\n")
        .stderr(contains("Running workflow"));

    Ok(())
}

#[test]
fn workflow_develop_refuses_to_overwrite_an_existing_target() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = project.path().join(".codex/workflows/reports/jira");
    fs::create_dir_all(&workflow_dir)?;
    let marker = workflow_dir.join("keep-me.txt");
    fs::write(&marker, "original")?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args([
        "workflow",
        "develop",
        "--location",
        "project",
        "--id",
        "reports/jira",
        "Prepare Jira summaries",
    ])
    .assert()
    .failure()
    .stderr(contains("already exists"));

    assert_eq!(fs::read_to_string(marker)?, "original");
    assert!(!workflow_dir.join("workflow.yaml").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_develop_refuses_to_traverse_a_symlink() -> Result<()> {
    use std::os::unix::fs::symlink;

    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    let outside = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflows_root = project.path().join(".codex/workflows");
    fs::create_dir_all(&workflows_root)?;
    symlink(outside.path(), workflows_root.join("reports"))?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args([
        "workflow",
        "develop",
        "--location",
        "project",
        "--id",
        "reports/jira",
        "Prepare Jira summaries",
    ])
    .assert()
    .failure()
    .stderr(contains("symbolic link"));

    assert!(!outside.path().join("jira").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_fix_repairs_workflow_without_running_unsupported_fix_action() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        "command: code-review\nuserDescription: Run a code review workflow.\n",
    )?;
    write_workflow_source(&workflow_dir)?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "fix", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "Repairing workflow code-review with compatibility mode.",
        ))
        .stdout(contains("Updated "))
        .stdout(contains("with code-review workflow metadata"))
        .stdout(contains("code-review repair check completed."));

    assert!(!fake_bun.was_invoked());
    assert_code_review_static_metadata_without_legacy_usage(&workflow_dir)?;

    Ok(())
}

#[test]
fn workflow_fix_keeps_valid_code_review_workflow_without_usage_options() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_yaml = r#"id: code-review
command: code-review
title: "/code-review"
userDescription: "Run a code review and repro workflow on the current branch."
"#;
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "code-review",
        workflow_yaml,
    )?;
    write_workflow_source(&workflow_dir)?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "fix", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "No compatibility repairs were needed for code-review.",
        ))
        .stdout(contains("code-review repair check completed."));

    assert_eq!(
        fs::read_to_string(workflow_dir.join("workflow.yaml"))?,
        workflow_yaml
    );
    assert_code_review_static_metadata_without_legacy_usage(&workflow_dir)?;

    Ok(())
}

#[test]
fn workflow_fix_rejects_runtime_arguments() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "fix", "code-review", "--scope", "repo"])
        .assert()
        .failure()
        .stderr(contains("unexpected argument '--scope'"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_fix_tolerates_broken_metadata_and_source_without_running_workflow() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = codex_home.path().join("workflows").join("code-review");
    fs::create_dir_all(workflow_dir.join("src"))?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: code-review\ncommand: [\n",
    )?;
    fs::write(
        workflow_dir.join("src").join("workflow.ts"),
        "export default async function workflow( {",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "fix", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "Repairing workflow code-review with compatibility mode.",
        ))
        .stdout(contains("Updated "))
        .stdout(contains("with code-review workflow metadata"))
        .stdout(contains("code-review repair check completed."));

    assert!(!fake_bun.was_invoked());
    assert_code_review_static_metadata_without_legacy_usage(&workflow_dir)?;

    Ok(())
}

#[test]
fn workflow_fix_scaffolds_missing_workflow_source_for_discovery_fallback() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_dir = codex_home.path().join("workflows").join("code-review");
    fs::create_dir_all(&workflow_dir)?;
    fs::write(
        workflow_dir.join("workflow.yaml"),
        "id: code-review\ncommand: [\n",
    )?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    cmd.args(["workflow", "fix", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "Repairing workflow code-review with compatibility mode.",
        ))
        .stdout(contains("Created "))
        .stdout(contains("src/workflow.ts"))
        .stdout(contains("code-review repair check completed."));

    assert!(workflow_dir.join("src").join("workflow.ts").is_file());
    assert_code_review_static_metadata_without_legacy_usage(&workflow_dir)?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_repair_alias_repairs_workflow_without_running_workflow_runtime() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    let workflow_yaml =
        "id: review\ncommand: code-review\nuserDescription: Run a code review workflow.\n";
    let workflow_dir = write_workflow(
        &codex_home.path().join("workflows"),
        "review",
        workflow_yaml,
    )?;
    write_workflow_source(&workflow_dir)?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "repair", "code-review"])
        .assert()
        .success()
        .stdout(contains(
            "Repairing workflow review with compatibility mode.",
        ))
        .stdout(contains("No compatibility repairs were needed for review."))
        .stdout(contains("review repair check completed."));

    assert!(!fake_bun.was_invoked());
    assert_eq!(
        fs::read_to_string(workflow_dir.join("workflow.yaml"))?,
        workflow_yaml
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn workflow_recover_normalizes_input_and_applies_recovery_fields() -> Result<()> {
    let codex_home = TempDir::new()?;
    let project = TempDir::new()?;
    enable_workflows(codex_home.path())?;
    scaffold_canonical_workflow(
        codex_home.path(),
        project.path(),
        "code-review",
        "code-review",
    )?;
    let fake_bun = FakeBun::new(codex_home.path())?;

    let mut cmd = codex_command(codex_home.path(), project.path())?;
    fake_bun.apply_to_command(&mut cmd)?;
    cmd.args(["workflow", "recover", "code-review", "--failure-id", "abc"])
        .assert()
        .success();

    assert_eq!(
        fake_bun.captured_workflow_input()?,
        json!({
            "action": "resume",
            "reviewId": "abc",
            "workingDirectory": existing_path_display(project.path())?,
        })
    );

    Ok(())
}
