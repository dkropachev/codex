use super::*;
use codex_config::config_toml::ContextHandoffRateToml;

fn config(soft_when: &str, mid_when: &str, hard_when: &str) -> ContextHandoffToml {
    ContextHandoffToml {
        soft_when: soft_when.to_string(),
        mid_when: mid_when.to_string(),
        hard_when: hard_when.to_string(),
        ..Default::default()
    }
}

#[test]
fn expression_precedence_and_missing_price() {
    let policy = ContextHandoffConfig::parse(&config(
        "context.size.tokens >= 10 || context.size.price >= 1 && context.size.tokens >= 5",
        "context.size.tokens >= 20",
        "context.size.tokens >= 30",
    ))
    .unwrap();
    assert_eq!(policy.stage(/*tokens*/ 4, Some(2.0)), Stage::None);
    assert_eq!(policy.stage(/*tokens*/ 5, Some(2.0)), Stage::Soft);
    assert_eq!(policy.stage(/*tokens*/ 10, None), Stage::Soft);
    assert_eq!(policy.stage(/*tokens*/ 20, None), Stage::Mid);
    assert_eq!(policy.stage(/*tokens*/ 30, None), Stage::Hard);
}

#[test]
fn invalid_expressions_are_rejected() {
    for expression in [
        "",
        "context.size.tokens >",
        "context.size.price > -1",
        "(context.size.tokens > 2",
        "context.size.tokens > 1 garbage",
    ] {
        assert!(
            ContextHandoffConfig::parse(&config(
                expression,
                "context.size.tokens > 2",
                "context.size.tokens > 3"
            ))
            .is_err()
        );
    }
}

#[test]
fn price_overrides_and_long_context_boundary() {
    let mut input = config(
        "context.size.price >= 1",
        "context.size.price >= 2",
        "context.size.price >= 3",
    );
    input.rates = Some(BTreeMap::from([(
        "openai/custom/default".to_string(),
        ContextHandoffRateToml {
            input_per_million: 10.0,
            cached_input_per_million: 1.0,
        },
    )]));
    let policy = ContextHandoffConfig::parse(&input).unwrap();
    assert_eq!(
        policy.price(
            "openai", "custom", "default", /*tokens*/ 100_000, /*cached*/ 50_000
        ),
        Some(0.55)
    );
    assert_eq!(
        policy.price(
            "openai", "custom", "default", /*tokens*/ 272_001, /*cached*/ 0
        ),
        Some(2.72001)
    );
    assert_eq!(
        policy.price(
            "openai",
            "gpt-6.1-sol",
            "default",
            /*tokens*/ 272_001,
            /*cached*/ 0
        ),
        Some(1.088004)
    );
    assert_eq!(
        policy.price(
            "other", "unknown", "default", /*tokens*/ 1_000_000, /*cached*/ 0
        ),
        None
    );
}
