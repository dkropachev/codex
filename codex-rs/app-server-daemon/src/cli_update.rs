//! Copies the invoking CLI package using the requested daemon update policy.

use anyhow::Result;
use codex_install_context::InstallContext;

use crate::Daemon;
use crate::managed_install;
use crate::prepare_install::InstallMode;
use crate::prepare_install::InstallRequest;
use crate::prepare_install::prepare_from_package;

/// Whether copying the CLI pins the daemon or refreshes an existing latest selection.
#[derive(Clone, Copy)]
pub enum CliUpdatePolicy {
    /// Explicitly replace the daemon and pin its package.
    Pin,
    /// Refresh only a daemon already following latest, preserving that channel.
    FollowLatest,
}

/// Select this CLI's complete package, restarting only a running daemon.
/// Returns None when the user cancels without changing the installation.
pub async fn update_from_cli(
    policy: CliUpdatePolicy,
    confirm: impl FnOnce(&InstallRequest) -> Result<bool>,
) -> Result<Option<crate::UpdateOutput>> {
    crate::ensure_supported_platform()?;
    #[cfg(windows)]
    crate::backend::windows::ensure_not_elevated()?;
    let daemon = Daemon::from_environment()?;
    let settings = daemon.load_settings().await?;
    let source = InstallContext::current().package_layout.as_ref();
    if !Box::pin(prepare_from_package(
        &daemon,
        &settings,
        match policy {
            CliUpdatePolicy::Pin => InstallMode::Replace,
            CliUpdatePolicy::FollowLatest => InstallMode::RefreshLatest,
        },
        source.map(|layout| layout.package_dir.as_path()),
        &std::env::current_exe()?,
        confirm,
    ))
    .await?
    {
        return Ok(None);
    }
    let managed_codex_path = daemon.current_managed_codex_bin()?;
    Ok(Some(crate::UpdateOutput {
        status: crate::UpdateStatus::Updated,
        installed_version: Some(managed_install::managed_codex_version(&managed_codex_path).await?),
        running_version: crate::client::probe(&daemon.socket_path)
            .await
            .ok()
            .map(|info| info.app_server_version),
        managed_codex_path,
        message: match policy {
            CliUpdatePolicy::Pin => "The CLI package is selected and pinned. Run `codex app-server daemon update` to return to production updates.",
            CliUpdatePolicy::FollowLatest => "The daemon follows the latest CLI package.",
        }.to_string(),
    }))
}
