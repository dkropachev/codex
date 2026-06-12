#![cfg(any(not(debug_assertions), test))]

use crate::legacy_core::config::Config;
#[cfg(not(debug_assertions))]
use crate::update_versions::extract_version_from_latest_tag;
use crate::update_versions::is_newer;
use crate::update_versions::is_source_build_version;
use chrono::DateTime;
use chrono::Utc;
#[cfg(not(debug_assertions))]
use codex_login::default_client::create_client;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(debug_assertions))]
use std::time::Duration;
#[cfg(not(debug_assertions))]
use tokio::time::MissedTickBehavior;

use crate::version::CODEX_CLI_VERSION;

#[cfg(not(debug_assertions))]
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn get_upgrade_version(config: &Config) -> Option<String> {
    get_upgrade_version_for_current(config, CODEX_CLI_VERSION)
}

fn get_upgrade_version_for_current(config: &Config, current_version: &str) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(current_version) {
        return None;
    }

    let version_file = version_filepath(config);
    let info = read_version_info(&version_file).ok();

    info.and_then(|info| {
        if is_newer(&info.latest_version, current_version).unwrap_or(false) {
            Some(info.latest_version)
        } else {
            None
        }
    })
}

#[cfg(not(debug_assertions))]
pub(crate) fn spawn_background_update_checker(config: &Config) {
    if !config.check_for_update_on_startup || is_source_build_version(CODEX_CLI_VERSION) {
        return;
    }

    let version_file = version_filepath(config);
    tokio::spawn(async move {
        refresh_cached_update_version(&version_file).await;

        let mut interval = tokio::time::interval(UPDATE_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // The first tick completes immediately; consume it because the startup
        // refresh has already run.
        interval.tick().await;

        loop {
            interval.tick().await;
            refresh_cached_update_version(&version_file).await;
        }
    });
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct VersionInfo {
    latest_version: String,
    // ISO-8601 timestamp (RFC3339)
    last_checked_at: DateTime<Utc>,
    #[serde(default)]
    dismissed_version: Option<String>,
}

const VERSION_FILENAME: &str = "version.json";
#[cfg(not(debug_assertions))]
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/dkropachev/codex/releases/latest";

#[cfg(not(debug_assertions))]
#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

fn version_filepath(config: &Config) -> PathBuf {
    config.codex_home.join(VERSION_FILENAME).into_path_buf()
}

fn read_version_info(version_file: &Path) -> anyhow::Result<VersionInfo> {
    let contents = std::fs::read_to_string(version_file)?;
    Ok(serde_json::from_str(&contents)?)
}

#[cfg(not(debug_assertions))]
async fn refresh_cached_update_version(version_file: &Path) {
    if let Err(err) = check_for_update(version_file).await {
        tracing::error!("Failed to update version: {err}");
    }
}

#[cfg(not(debug_assertions))]
async fn check_for_update(version_file: &Path) -> anyhow::Result<()> {
    let latest_version = fetch_latest_github_release_version().await?;
    // Preserve any previously dismissed version if present.
    let prev_info = read_version_info(version_file).ok();
    let info = VersionInfo {
        latest_version,
        last_checked_at: Utc::now(),
        dismissed_version: prev_info.and_then(|p| p.dismissed_version),
    };

    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

#[cfg(not(debug_assertions))]
async fn fetch_latest_github_release_version() -> anyhow::Result<String> {
    let ReleaseInfo {
        tag_name: latest_tag_name,
    } = create_client()
        .get(LATEST_RELEASE_URL)
        .send()
        .await?
        .error_for_status()?
        .json::<ReleaseInfo>()
        .await?;
    extract_version_from_latest_tag(&latest_tag_name)
}

/// Returns the latest version to show in a popup, if it should be shown.
/// This respects the user's dismissal choice for the current latest version.
pub fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    get_upgrade_version_for_popup_for_current(config, CODEX_CLI_VERSION)
}

fn get_upgrade_version_for_popup_for_current(
    config: &Config,
    current_version: &str,
) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(current_version) {
        return None;
    }

    let version_file = version_filepath(config);
    let latest = get_upgrade_version_for_current(config, current_version)?;
    // If the user dismissed this exact version previously, do not show the popup.
    if let Ok(info) = read_version_info(&version_file)
        && info.dismissed_version.as_deref() == Some(latest.as_str())
    {
        return None;
    }
    Some(latest)
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub async fn dismiss_version(config: &Config, version: &str) -> anyhow::Result<()> {
    let version_file = version_filepath(config);
    let mut info = match read_version_info(&version_file) {
        Ok(info) => info,
        Err(_) => return Ok(()),
    };
    info.dismissed_version = Some(version.to_string());
    let json_line = format!("{}\n", serde_json::to_string(&info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
