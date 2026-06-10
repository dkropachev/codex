use std::path::Path;

use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

fn write_native_workflow_config(codex_home: &Path) -> Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        r#"model = "gpt-oss:20b"
model_provider = "ollama"
check_for_update_on_startup = false
suppress_unstable_features_warning = true

[analytics]
enabled = false

[workflows.engines.rust]
enabled = true
"#,
    )?;
    Ok(())
}

#[test]
fn native_workflow_list_and_run_dev_cycle() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_native_workflow_config(codex_home.path())?;

    let output = codex_command(codex_home.path())?
        .args(["native-workflow", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let workflows = serde_json::from_slice::<JsonValue>(&output)?;

    assert_eq!(workflows[0]["id"], json!("dev-cycle"));
    assert_eq!(workflows[0]["engine"], json!("rust"));
    assert_eq!(workflows[0]["title"], json!("Development Cycle"));

    let output = codex_command(codex_home.path())?
        .args([
            "native-workflow",
            "run",
            "dev-cycle",
            "--input",
            r#"{"stageTests":"off"}"#,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let run_output = serde_json::from_slice::<JsonValue>(&output)?;

    assert_eq!(run_output["status"], json!("blocked"));
    assert_eq!(
        run_output["blockedReason"],
        json!("native agent runtime is unavailable")
    );
    assert_eq!(run_output["settings"]["stages"]["tests"], json!("off"));

    Ok(())
}
