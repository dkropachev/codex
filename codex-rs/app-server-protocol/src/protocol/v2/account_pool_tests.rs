use super::*;
use codex_protocol::account::ProviderAccountPoolMember;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn pool_resources_preserve_nullability_and_exact_balances() {
    for credits in [
        None,
        Some(CoreCreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: None,
        }),
        Some(CoreCreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("12.340000000000000001".to_string()),
        }),
        Some(CoreCreditsSnapshot {
            has_credits: false,
            unlimited: true,
            balance: None,
        }),
        Some(CoreCreditsSnapshot {
            has_credits: false,
            unlimited: false,
            balance: Some("0".to_string()),
        }),
    ] {
        let account = Account::from(ProviderAccount::ChatgptPool {
            id: "pool".to_string(),
            active_account_id: None,
            members: vec![ProviderAccountPoolMember {
                id: "work".to_string(),
                email: None,
                plan_type: None,
                active: false,
                unavailable_reason: None,
                regular_remaining: None,
                spark_remaining: None,
                credits: credits.clone(),
                rate_limit_reset_credit_count: Some(3),
                last_error: None,
            }],
        });
        let wire = serde_json::to_value(&account).unwrap();
        assert_eq!(
            wire,
            json!({"type":"chatgptPool","id":"pool","activeAccountId":null,"members":[{
                "id":"work","email":null,"planType":null,"active":false,"unavailableReason":null,
                "regularRemaining":null,"sparkRemaining":null,"lastError":null,
                "credits":credits.map(|credits| json!({"hasCredits":credits.has_credits,"unlimited":credits.unlimited,"balance":credits.balance})),
                "rateLimitResetCreditCount":3
            }]})
        );
        assert_eq!(serde_json::from_value::<Account>(wire).unwrap(), account);
    }
    let legacy = json!({"id":"work","email":null,"planType":null,"active":false,"unavailableReason":null,"regularRemaining":null,"sparkRemaining":null,"lastError":null});
    let member = serde_json::from_value::<AccountPoolMember>(legacy).unwrap();
    assert_eq!(
        (member.credits, member.rate_limit_reset_credit_count),
        (None, None)
    );
}
