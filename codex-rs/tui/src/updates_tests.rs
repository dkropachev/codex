use super::*;
use crate::legacy_core::config::ConfigBuilder;
use chrono::DateTime;
use chrono::Utc;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tempfile::tempdir;

async fn test_config(codex_home: &TempDir) -> Config {
    ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("load config")
}

async fn write_cached_version(config: &Config, info: &VersionInfo) {
    let version_file = version_filepath(config);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .expect("create version cache parent");
    }
    let json_line = format!(
        "{}\n",
        serde_json::to_string(info).expect("serialize cache")
    );
    tokio::fs::write(version_file, json_line)
        .await
        .expect("write version cache");
}

fn version_info(latest_version: &str, dismissed_version: Option<&str>) -> VersionInfo {
    VersionInfo {
        latest_version: latest_version.to_string(),
        last_checked_at: DateTime::<Utc>::UNIX_EPOCH,
        dismissed_version: dismissed_version.map(str::to_string),
    }
}

#[tokio::test]
async fn cached_upgrade_version_is_returned_without_refreshing_stale_cache() {
    let codex_home = tempdir().expect("temp codex home");
    let config = test_config(&codex_home).await;
    write_cached_version(&config, &version_info("9.9.9", /*dismissed_version*/ None)).await;

    assert_eq!(
        get_upgrade_version_for_current(&config, "1.2.3"),
        Some("9.9.9".to_string())
    );
}

#[tokio::test]
async fn source_build_version_skips_public_update_checks() {
    let codex_home = tempdir().expect("temp codex home");
    let config = test_config(&codex_home).await;
    write_cached_version(&config, &version_info("9.9.9", /*dismissed_version*/ None)).await;

    assert_eq!(get_upgrade_version(&config), None);
    assert_eq!(get_upgrade_version_for_popup(&config), None);
}

#[tokio::test]
async fn popup_uses_cached_version_when_not_dismissed() {
    let codex_home = tempdir().expect("temp codex home");
    let config = test_config(&codex_home).await;
    write_cached_version(&config, &version_info("9.9.9", Some("9.9.8"))).await;

    assert_eq!(
        get_upgrade_version_for_popup_for_current(&config, "1.2.3"),
        Some("9.9.9".to_string())
    );
}

#[tokio::test]
async fn popup_ignores_cached_version_when_dismissed() {
    let codex_home = tempdir().expect("temp codex home");
    let config = test_config(&codex_home).await;
    write_cached_version(&config, &version_info("9.9.9", Some("9.9.9"))).await;

    assert_eq!(
        get_upgrade_version_for_popup_for_current(&config, "1.2.3"),
        None
    );
}

#[tokio::test]
async fn dismiss_version_persists_dismissed_version() {
    let codex_home = tempdir().expect("temp codex home");
    let config = test_config(&codex_home).await;
    write_cached_version(&config, &version_info("9.9.9", /*dismissed_version*/ None)).await;

    dismiss_version(&config, "9.9.9")
        .await
        .expect("dismiss version");

    assert_eq!(
        read_version_info(&version_filepath(&config)).expect("read version cache"),
        version_info("9.9.9", Some("9.9.9"))
    );
}
