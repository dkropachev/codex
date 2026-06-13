use anyhow::Result;
use predicates::str::contains;
use std::path::Path;
use tempfile::TempDir;

fn codex_command(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?);
    cmd.env("CODEX_HOME", codex_home);
    Ok(cmd)
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn update_prints_github_download_url() -> Result<()> {
    let codex_home = TempDir::new()?;

    codex_command(codex_home.path())?
        .arg("update")
        .assert()
        .success()
        .stdout(contains(
            "Download the latest Codex release from https://github.com/dkropachev/codex/releases/latest",
        ));

    Ok(())
}
