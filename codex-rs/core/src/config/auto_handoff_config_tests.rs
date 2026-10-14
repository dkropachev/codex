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

#[tokio::test]
async fn context_handoff_validates_conditions_and_conflicts() -> std::io::Result<()> {
    let policy = "[context_handoff]\nsoft_when = 'context.size.tokens > 100'\nmid_when = 'context.size.tokens > 200'\nhard_when = 'context.size.tokens > 300'";
    for (toml, expected_error) in [
        (
            format!("{policy}\n[tui]\nauto_handoff_threshold_percent = 80"),
            "context_handoff cannot be combined with tui.auto_handoff_threshold_percent",
        ),
        (
            policy.replace("context.size.tokens > 300", "context.size.tokens >"),
            "context_handoff.hard_when: expected nonnegative number",
        ),
    ] {
        let cfg = toml::from_str::<ConfigToml>(&toml).unwrap();
        let codex_home = TempDir::new()?;
        let err = Config::load_from_base_config_with_overrides(
            cfg,
            ConfigOverrides::default(),
            codex_home.abs(),
        )
        .await
        .expect_err("invalid handoff policy should fail to load");
        assert_eq!(
            (err.kind(), err.to_string()),
            (std::io::ErrorKind::InvalidInput, expected_error.to_string())
        );
    }
    Ok(())
}

#[tokio::test]
async fn context_handoff_loads_rates_and_guidance_from_toml() -> std::io::Result<()> {
    let cfg = toml::from_str::<ConfigToml>(
        "[context_handoff]\nsoft_when = 'context.size.tokens >= 100'\nmid_when = 'context.size.tokens >= 200'\nhard_when = 'context.size.price >= 1'\nsoft_guidance = 'Save a checkpoint soon.'\n[context_handoff.rates.\"mock/model/default\"]\ninput_per_million = 4.0\ncached_input_per_million = 1.0",
    )
    .expect("context handoff config should deserialize");
    let codex_home = TempDir::new()?;
    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        codex_home.abs(),
    )
    .await?;
    let handoff = config
        .context_handoff
        .expect("context handoff should be enabled");
    assert_eq!(handoff.soft_guidance, "Save a checkpoint soon.");
    assert_eq!(
        handoff.price(
            "mock", "model", "default", /*tokens*/ 1_000_000, /*cached*/ 500_000
        ),
        Some(2.5),
    );
    Ok(())
}
