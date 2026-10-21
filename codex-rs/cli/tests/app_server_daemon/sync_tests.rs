//! Exercises installer-triggered package synchronization through the CLI.
use super::TestDaemon;
use super::wait_for_exit;
use anyhow::Result;
use anyhow::ensure;
use codex_utils_cargo_bin::copy_executable;
use pretty_assertions::assert_eq;
use serde_json::Value;

#[test]
fn latest_cli_restarts_the_daemon_with_its_matching_host_and_keeps_updates_enabled() -> Result<()> {
    let mut daemon = TestDaemon::new()?;
    std::fs::rename(
        daemon.home.path().join("packages/standalone"),
        daemon.home.path().join("packages/app-server-daemon"),
    )?;
    daemon.lifecycle("start")?;
    let original_pid = daemon.pid("daemon.pid")?;
    let home = daemon.home.path().canonicalize()?;
    let standalone = home.join("packages/standalone");
    let target = format!(
        "{}-{}",
        std::env::consts::ARCH,
        if cfg!(target_os = "macos") {
            "apple-darwin"
        } else if cfg!(target_env = "gnu") {
            "unknown-linux-gnu"
        } else {
            "unknown-linux-musl"
        }
    );
    let name = format!("dkropachev-{}-{target}", env!("CARGO_PKG_VERSION"));
    let package = standalone.join("releases").join(&name);
    for directory in ["bin", "codex-path", "codex-resources"] {
        std::fs::create_dir_all(package.join(directory))?;
    }
    copy_executable(&daemon.codex, &package.join("bin/codex"))?;
    daemon.codex = package.join("bin/codex");
    for helper in [
        "bin/codex-code-mode-host",
        "codex-path/rg",
        "codex-resources/bwrap",
    ] {
        codex_utils_cargo_bin::write_executable(&package.join(helper), "new runtime")?;
    }
    std::fs::write(
        package.join("codex-package.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"), "target": target, "entrypoint": "bin/codex"
        }))?,
    )?;
    std::os::unix::fs::symlink(&package, standalone.join("current"))?;
    std::fs::write(standalone.join("auto-update-version"), &name)?;
    let output = daemon
        .command()
        .args(["app-server", "daemon", "sync-from-cli"])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout)?;
    let selected = home
        .join("packages/app-server-daemon/current")
        .canonicalize()?;
    let host = codex_install_context::InstallContext::from_exe(
        cfg!(target_os = "macos"),
        Some(&selected.join("bin/codex")),
        /*method_override*/ None,
    )
    .code_mode_host_program();
    assert_eq!(
        (
            result["status"].as_str(),
            selected.file_name(),
            host.clone(),
            std::fs::read(&host)?
        ),
        (
            Some("updated"),
            Some(std::ffi::OsStr::new(&name)),
            selected.join("bin/codex-code-mode-host"),
            b"new runtime".to_vec()
        )
    );
    assert_ne!(daemon.pid("daemon.pid")?, original_pid);
    wait_for_exit(original_pid)?;
    let updater = daemon.pid("daemon-updater.pid")?;
    assert_eq!(
        std::fs::read_to_string(home.join("packages/app-server-daemon/auto-update-version"))?,
        name
    );
    assert_eq!(standalone.join("current").canonicalize()?, package);
    let pid = daemon.pid("daemon.pid")?;
    let retry = daemon
        .command()
        .args(["app-server", "daemon", "sync-from-cli"])
        .output()?;
    ensure!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        (daemon.pid("daemon.pid")?, daemon.pid("daemon-updater.pid")?),
        (pid, updater)
    );
    daemon.lifecycle("stop")?;
    // A pinned daemon is deliberately left alone by subsequent CLI upgrades.
    std::fs::remove_file(home.join("packages/app-server-daemon/auto-update-version"))?;
    let pinned = daemon
        .command()
        .args(["app-server", "daemon", "sync-from-cli"])
        .output()?;
    assert!(pinned.status.success());
    assert!(pinned.stdout.is_empty());
    assert_eq!(
        home.join("packages/app-server-daemon/current")
            .canonicalize()?,
        selected
    );
    Ok(())
}
