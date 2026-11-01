use super::routing::CapacityTier;
use super::tests::pool_manager_with_base_url;
use super::tests::write_chatgpt_auth;
use super::*;
use codex_config::config_toml::AccountPoolFallbackPriorityToml;
use pretty_assertions::assert_eq;
use serde_json::json;

async fn resource_pool(policy: AccountPoolPolicyToml) -> (tempfile::TempDir, AccountPoolManager) {
    let home = tempfile::tempdir().unwrap();
    write_chatgpt_auth(home.path(), "work-pro", "work@example.com");
    write_chatgpt_auth(home.path(), "personal-pro", "personal@example.com");
    let manager = pool_manager_with_base_url(
        home.path(),
        policy,
        vec!["work-pro", "personal-pro"],
        /*chatgpt_base_url*/ None,
    )
    .await;
    (home, manager)
}

fn set_resources(pool: &AccountPool, id: &str, payload: serde_json::Value) {
    pool.apply_usage(id, usage::parse_usage(&payload).unwrap());
}

#[tokio::test]
async fn permissions_and_priority_choose_only_enabled_resources_for_both_policies() {
    for policy in [
        AccountPoolPolicyToml::Drain,
        AccountPoolPolicyToml::LoadBalance,
    ] {
        let (_home, mut manager) = resource_pool(policy).await;
        for resets in [false, true] {
            for credits in [false, true] {
                for priority in [
                    AccountPoolFallbackPriorityToml::ResetsFirst,
                    AccountPoolFallbackPriorityToml::CreditsFirst,
                ] {
                    let pool = manager.pools.get_mut("codex-pro").unwrap();
                    pool.definition.auto_consume_resets = resets;
                    pool.definition.auto_consume_credits = credits;
                    pool.definition.fallback_priority = priority;
                    set_resources(
                        pool,
                        "work-pro",
                        json!({"rate_limit":{"allowed":false}, "rate_limit_reset_credits":{"available_count":3}}),
                    );
                    set_resources(
                        pool,
                        "personal-pro",
                        json!({"rate_limit":{"allowed":false}, "credits":{"has_credits":true, "unlimited":false, "balance":"12.3400"}}),
                    );
                    let expected = match (resets, credits, priority) {
                        (false, false, _) => None,
                        (true, false, _)
                        | (true, true, AccountPoolFallbackPriorityToml::ResetsFirst) => {
                            Some("work-pro")
                        }
                        (false, true, _)
                        | (true, true, AccountPoolFallbackPriorityToml::CreditsFirst) => {
                            Some("personal-pro")
                        }
                    };
                    assert_eq!(
                        manager
                            .auth_for_context(AccountPoolSelectionContext::default_for_bucket(
                                AccountPoolUsageBucket::Regular
                            ))
                            .await
                            .map(|selection| selection.account_id),
                        expected.map(str::to_string)
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn included_quota_wins_over_fallback_affinity_and_drain_order() {
    for policy in [
        AccountPoolPolicyToml::Drain,
        AccountPoolPolicyToml::LoadBalance,
    ] {
        let (_home, mut manager) = resource_pool(policy).await;
        let pool = manager.pools.get_mut("codex-pro").unwrap();
        pool.definition.auto_consume_resets = true;
        pool.definition.auto_consume_credits = true;
        set_resources(
            pool,
            "work-pro",
            json!({"rate_limit":{"allowed":false}, "rate_limit_reset_credits":{"available_count":10}, "credits":{"has_credits":true,"unlimited":true}}),
        );
        set_resources(
            pool,
            "personal-pro",
            json!({"rate_limit":{"allowed":true,"primary_window":{"used_percent":95}}}),
        );
        let mut context =
            AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
        context.cache_hint = Some(AccountPoolCacheHint {
            input_tokens: 20_000,
            cached_input_tokens: 20_000,
        });
        pool.set_assignment(
            pool.assignment_key(context.bucket, &context.affinity_key),
            "work-pro".to_string(),
            context.cache_hint,
        );
        assert_eq!(
            manager.auth_for_context(context).await.unwrap().account_id,
            "personal-pro"
        );
    }
}

#[tokio::test]
async fn cold_fallback_ordering_preserves_warm_and_hot_affinity() {
    let (_home, mut manager) = resource_pool(AccountPoolPolicyToml::LoadBalance).await;
    let pool = manager.pools.get_mut("codex-pro").unwrap();
    pool.definition.auto_consume_resets = true;
    pool.definition.auto_consume_credits = true;
    for (priority, work, personal) in [
        (
            AccountPoolFallbackPriorityToml::ResetsFirst,
            json!({"rate_limit":{"allowed":false},"rate_limit_reset_credits":{"available_count":1}}),
            json!({"rate_limit":{"allowed":false},"rate_limit_reset_credits":{"available_count":5}}),
        ),
        (
            AccountPoolFallbackPriorityToml::CreditsFirst,
            json!({"rate_limit":{"allowed":false},"credits":{"has_credits":true,"unlimited":false,"balance":"9"}}),
            json!({"rate_limit":{"allowed":false},"credits":{"has_credits":true,"unlimited":false,"balance":"100.00000000000000000001"}}),
        ),
    ] {
        pool.definition.fallback_priority = priority;
        set_resources(pool, "work-pro", work);
        set_resources(pool, "personal-pro", personal);
        let context =
            AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
        for hint in [
            AccountPoolCacheHint::default(),
            AccountPoolCacheHint {
                input_tokens: 10_000,
                cached_input_tokens: 3_000,
            },
            AccountPoolCacheHint {
                input_tokens: 20_000,
                cached_input_tokens: 20_000,
            },
        ] {
            pool.set_assignment(
                pool.assignment_key(context.bucket, &context.affinity_key),
                "work-pro".to_string(),
                Some(hint),
            );
            let expected = if hint.heat() == AccountPoolCacheHeat::Cold {
                vec!["personal-pro", "work-pro"]
            } else {
                vec!["work-pro", "personal-pro"]
            };
            assert_eq!(pool.select_member_ids(&context, Some(hint)), expected);
        }
    }
}

#[tokio::test]
async fn credit_states_and_restrictions_remain_distinct() {
    let (_home, mut manager) = resource_pool(AccountPoolPolicyToml::LoadBalance).await;
    let pool = manager.pools.get_mut("codex-pro").unwrap();
    pool.definition.auto_consume_credits = true;
    let bucket = AccountPoolUsageBucket::Regular;
    for (credits, reached, expected) in [
        (serde_json::Value::Null, false, None),
        (
            json!({"has_credits":true,"unlimited":false,"balance":null}),
            false,
            Some(CapacityTier::Credits),
        ),
        (
            json!({"has_credits":true,"unlimited":false,"balance":"0.000"}),
            false,
            None,
        ),
        (
            json!({"has_credits":false,"unlimited":true,"balance":null}),
            false,
            Some(CapacityTier::Credits),
        ),
        (
            json!({"has_credits":true,"unlimited":false,"balance":"12.3400"}),
            true,
            None,
        ),
    ] {
        set_resources(
            pool,
            "work-pro",
            json!({"rate_limit":{"allowed":false},"credits":credits,"spend_control":{"reached":reached}}),
        );
        assert_eq!(pool.member_tier("work-pro", bucket), expected);
        assert_eq!(
            pool.status().members[0]
                .credits
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .unwrap(),
            (!credits.is_null()).then_some(credits)
        );
    }
    set_resources(
        pool,
        "work-pro",
        json!({"credits":{"has_credits":false,"unlimited":false,"balance":"0"}}),
    );
    assert_eq!(
        pool.member_tier("work-pro", bucket),
        Some(CapacityTier::Included),
        "missing usage remains best effort"
    );
    pool.mark_exhausted("work-pro", bucket, "exhausted".to_string());
    set_resources(
        pool,
        "work-pro",
        json!({"rate_limit":{"allowed":false,"primary_window":{"used_percent":0}}}),
    );
    assert_eq!(
        pool.member_tier("work-pro", bucket),
        None,
        "display percentages cannot override backend permission"
    );
}

#[tokio::test]
async fn credit_order_is_numeric_and_drain_keeps_configuration_order() {
    for policy in [
        AccountPoolPolicyToml::Drain,
        AccountPoolPolicyToml::LoadBalance,
    ] {
        let (_home, mut manager) = resource_pool(policy).await;
        let pool = manager.pools.get_mut("codex-pro").unwrap();
        pool.definition.auto_consume_credits = true;
        for (left, right) in [
            (
                json!({"has_credits":true,"unlimited":false,"balance":"9"}),
                json!({"has_credits":true,"unlimited":false,"balance":"100"}),
            ),
            (
                json!({"has_credits":true,"unlimited":false,"balance":"100.00000000000000000001"}),
                json!({"has_credits":true,"unlimited":false,"balance":"100.00000000000000000002"}),
            ),
            (
                json!({"has_credits":true,"unlimited":false,"balance":null}),
                json!({"has_credits":true,"unlimited":false,"balance":"1"}),
            ),
            (
                json!({"has_credits":true,"unlimited":false,"balance":"1000"}),
                json!({"has_credits":false,"unlimited":true,"balance":null}),
            ),
        ] {
            set_resources(
                pool,
                "work-pro",
                json!({"rate_limit":{"allowed":false},"credits":left}),
            );
            set_resources(
                pool,
                "personal-pro",
                json!({"rate_limit":{"allowed":false},"credits":right}),
            );
            let context =
                AccountPoolSelectionContext::default_for_bucket(AccountPoolUsageBucket::Regular);
            let expected = match policy {
                AccountPoolPolicyToml::Drain => vec!["work-pro", "personal-pro"],
                AccountPoolPolicyToml::LoadBalance => vec!["personal-pro", "work-pro"],
            };
            assert_eq!(
                pool.select_member_ids(&context, /*cache_hint*/ None),
                expected
            );
        }
    }
}

#[tokio::test]
async fn legacy_usage_rounding_does_not_mistake_nearly_exhausted_quota_for_exhaustion() {
    let (_home, manager) = resource_pool(AccountPoolPolicyToml::Drain).await;
    let pool = &manager.pools["codex-pro"];
    for (percent, expected) in [(99.6, Some(CapacityTier::Included)), (100.0, None)] {
        set_resources(
            pool,
            "work-pro",
            json!({"rate_limit":{"primary_window":{"used_percent":percent}}}),
        );
        assert_eq!(pool.status().members[0].regular_remaining, Some(0));
        assert_eq!(
            pool.member_tier("work-pro", AccountPoolUsageBucket::Regular),
            expected
        );
    }
}
