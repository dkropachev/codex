use anyhow::Result;
use predicates::str::contains;
use std::path::Path;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

#[cfg(all(debug_assertions, not(windows)))]
#[tokio::test]
async fn update_reports_managed_fork_bootstrap_command() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .arg("update")
        .assert()
        .failure()
        .stderr(contains(
            "curl -fsSL https://github.com/dkropachev/codex/releases/latest/download/install.sh | sh",
        ));

    Ok(())
}

#[cfg(all(debug_assertions, windows))]
#[tokio::test]
async fn update_reports_that_windows_self_update_is_unsupported() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .arg("update")
        .assert()
        .failure()
        .stderr(contains(
            "the managed fork does not support Windows self-update",
        ))
        .stderr(contains(
            "https://github.com/dkropachev/codex/releases/latest/download/install.sh",
        ));

    Ok(())
}
