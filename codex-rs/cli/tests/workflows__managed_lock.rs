#![cfg(unix)]

use std::fs;
use std::fs::TryLockError;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use anyhow::Result;
use codex_workflows::ScaffoldRequest;
use codex_workflows::scaffold_workflow;
use tempfile::TempDir;

fn codex(home: &Path, cwd: &Path) -> Result<Command> {
    let mut command = Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    command
        .env("CODEX_HOME", home)
        .env("HOME", home)
        .current_dir(cwd);
    Ok(command)
}

#[test]
fn managed_run_holds_release_lock_through_completion() -> Result<()> {
    let home = TempDir::new()?;
    let project = TempDir::new()?;
    let source = scaffold_workflow(
        &project.path().join("sources"),
        &ScaffoldRequest {
            id: "team/build".into(),
            title: "Team Build".into(),
            callable_name: "team-build".into(),
            description: "Build workflow".into(),
        },
    )?;
    for args in [
        &["add", "--all"][..],
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"][..],
    ] {
        let output = Command::new("git")
            .current_dir(&source)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args)
            .output()?;
        anyhow::ensure!(output.status.success(), "Git failed");
    }
    fs::write(
        home.path().join("config.toml"),
        "[features]\nworkflows = true\n",
    )?;
    let installed = codex(home.path(), project.path())?
        .args([
            "workflow",
            "install",
            source
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("source path"))?,
        ])
        .output()?;
    anyhow::ensure!(
        installed.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );

    let bin = project.path().join("bin");
    fs::create_dir(&bin)?;
    let fake_bun = bin.join("bun");
    fs::write(
        &fake_bun,
        r#"#!/bin/sh
while [ "${1#--}" != "$1" ]; do shift; done
if [ "${2:-}" = "inspect" ]; then
  printf '%s\n' '{"apiVersion":1,"id":"team/build","title":"Team Build","callableName":"team-build","inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"hasComplete":true}'
fi
if [ "${2:-}" = "scan" ]; then
  printf '%s\n' '[{"path":"src/workflow.ts","exports":["inputSchema","outputSchema"],"imports":[]}]'
fi
if [ "${2:-}" = "run" ]; then
  : > "$CODEX_TEST_WORKFLOW_STARTED"
  while [ ! -f "$CODEX_TEST_WORKFLOW_RELEASE" ]; do sleep 0.05; done
  mkdir -p state artifacts
  printf '%s\n' 'runtime state' > state/session
  printf '%s\n' 'runtime artifact' > artifacts/report
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":1,"method":"contract","params":{"inputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true},"outputSchema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":true}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":2,"method":"validateOutput","params":{"output":{}}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
  printf '\036CODEX_WORKFLOW_CONTROL %s\n' '{"v":1,"id":0,"method":"complete","params":{"markdown":"done\n"}}' >> "$CODEX_WORKFLOW_CONTROL_PATH"
  IFS= read -r _response
fi
"#,
    )?;
    let mut permissions = fs::metadata(&fake_bun)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_bun, permissions)?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").ok_or_else(|| anyhow::anyhow!("PATH"))?,
    )))?;
    let started = project.path().join("started");
    let release = project.path().join("release");
    let mut running = codex(home.path(), project.path())?
        .env("PATH", &path)
        .env("CODEX_TEST_WORKFLOW_STARTED", &started)
        .env("CODEX_TEST_WORKFLOW_RELEASE", &release)
        .args(["workflow", "run", "team/build"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() {
        if Instant::now() >= deadline || running.try_wait()?.is_some() {
            let _ = running.kill();
            let output = running.wait_with_output()?;
            anyhow::bail!(
                "workflow did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    let lock_file = fs::OpenOptions::new().read(true).write(true).open(
        home.path()
            .join(".workflow-management/locks/team/build.lock"),
    )?;
    assert!(matches!(
        lock_file.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    let mut mutation = codex(home.path(), project.path())?
        .args(["workflow", "set-policy", "team/build", "manual"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    thread::sleep(Duration::from_millis(300));
    assert!(mutation.try_wait()?.is_none());
    fs::write(&release, "continue")?;
    let output = running.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "workflow failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let changed = mutation.wait_with_output()?;
    anyhow::ensure!(
        changed.status.success(),
        "policy change failed: {}",
        String::from_utf8_lossy(&changed.stderr)
    );
    lock_file.try_lock()?;
    drop(lock_file);
    anyhow::ensure!(
        !home
            .path()
            .join("workflows/team/build/state/session")
            .exists(),
        "runtime state changed the installed payload"
    );
    let second_run = codex(home.path(), project.path())?
        .env("PATH", &path)
        .env("CODEX_TEST_WORKFLOW_STARTED", &started)
        .env("CODEX_TEST_WORKFLOW_RELEASE", &release)
        .args(["workflow", "run", "team/build"])
        .output()?;
    anyhow::ensure!(
        second_run.status.success(),
        "second run failed: {}",
        String::from_utf8_lossy(&second_run.stderr)
    );
    let sibling = home.path().join("workflows/zz-override");
    fs::create_dir(&sibling)?;
    fs::copy(
        home.path().join("workflows/team/build/workflow.yaml"),
        sibling.join("workflow.yaml"),
    )?;
    let wrong_root = codex(home.path(), project.path())?
        .args(["workflow", "run", "team/build"])
        .output()?;
    anyhow::ensure!(!wrong_root.status.success(), "unmanaged sibling ran");
    anyhow::ensure!(
        String::from_utf8_lossy(&wrong_root.stderr).contains("not a canonical package"),
        "unexpected sibling error: {}",
        String::from_utf8_lossy(&wrong_root.stderr)
    );
    fs::remove_dir_all(sibling)?;
    fs::write(
        home.path().join("workflows/team/build/src/workflow.ts"),
        "modified locally\n",
    )?;
    let dirty = codex(home.path(), project.path())?
        .args(["workflow", "run", "team/build"])
        .output()?;
    anyhow::ensure!(!dirty.status.success(), "dirty workflow ran");
    anyhow::ensure!(
        String::from_utf8_lossy(&dirty.stderr).contains("active payload differs"),
        "unexpected dirty workflow error: {}",
        String::from_utf8_lossy(&dirty.stderr)
    );
    Ok(())
}
