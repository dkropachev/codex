use super::*;
use core_test_support::TempDirExt;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test]
async fn auto_handoff_threshold_is_optional_and_accepts_allowed_range() -> std::io::Result<()> {
    for (toml, expected) in [
        ("", None),
        ("[tui]\nauto_handoff_threshold_percent = 71", Some(71)),
        ("[tui]\nauto_handoff_threshold_percent = 80", Some(80)),
        ("[tui]\nauto_handoff_threshold_percent = 85", Some(85)),
    ] {
        let cfg =
            toml::from_str::<ConfigToml>(toml).expect("TUI handoff config should deserialize");
        let codex_home = TempDir::new()?;
        let config = Config::load_from_base_config_with_overrides(
            cfg,
            ConfigOverrides::default(),
            codex_home.abs(),
        )
        .await?;

        assert_eq!(config.tui_auto_handoff_threshold_percent, expected);
    }

    Ok(())
}

#[tokio::test]
async fn auto_handoff_threshold_rejects_values_outside_allowed_range() -> std::io::Result<()> {
    for threshold_percent in [0, 70, 86, 100] {
        let cfg = toml::from_str::<ConfigToml>(&format!(
            "[tui]\nauto_handoff_threshold_percent = {threshold_percent}"
        ))
        .expect("out-of-range TUI handoff config should deserialize before runtime validation");
        let codex_home = TempDir::new()?;
        let err = Config::load_from_base_config_with_overrides(
            cfg,
            ConfigOverrides::default(),
            codex_home.abs(),
        )
        .await
        .expect_err("out-of-range TUI handoff threshold should be rejected");

        assert_eq!(
            (err.kind(), err.to_string()),
            (
                std::io::ErrorKind::InvalidInput,
                "tui.auto_handoff_threshold_percent must be between 71 and 85".to_string(),
            ),
        );
    }

    Ok(())
}
