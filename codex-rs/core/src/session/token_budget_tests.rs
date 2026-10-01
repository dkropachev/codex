use super::experimental_context_is_eligible;
use codex_protocol::account::PlanType;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;

#[test]
fn experimental_context_requires_eligible_chatgpt_subscription() {
    for (auth_mode, plan_type, expected) in [
        (AuthMode::Chatgpt, PlanType::Plus, true),
        (AuthMode::Chatgpt, PlanType::Pro, true),
        (AuthMode::Chatgpt, PlanType::ProLite, true),
        (AuthMode::Chatgpt, PlanType::Free, false),
        (AuthMode::Chatgpt, PlanType::Enterprise, false),
        (AuthMode::ApiKey, PlanType::Pro, false),
    ] {
        assert_eq!(
            experimental_context_is_eligible(auth_mode, Some(plan_type)),
            expected
        );
    }
}
