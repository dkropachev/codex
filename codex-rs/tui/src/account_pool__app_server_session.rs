use super::account_ui_state_from_response;
use crate::status::StatusAccountDisplay;
use crate::status::StatusAccountPoolMemberDisplay;
use codex_app_server_protocol::Account;
use codex_app_server_protocol::GetAccountResponse;
use pretty_assertions::assert_eq;

#[test]
fn account_ui_state_from_response_preserves_chatgpt_pool_details() {
    let response = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: Some("work-pro".to_string()),
            members: vec![
                codex_app_server_protocol::AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(codex_protocol::account::PlanType::Pro),
                    active: true,
                    unavailable_reason: None,
                    regular_remaining: Some(80),
                    spark_remaining: Some(50),
                    last_error: None,
                },
                codex_app_server_protocol::AccountPoolMember {
                    id: "personal-pro".to_string(),
                    email: None,
                    plan_type: None,
                    active: false,
                    unavailable_reason: Some("missing credentials".to_string()),
                    regular_remaining: None,
                    spark_remaining: None,
                    last_error: Some("missing credentials".to_string()),
                },
            ],
        }),
        requires_openai_auth: true,
    };

    let account_ui = account_ui_state_from_response(&response);

    assert_eq!(
        account_ui.account_email.as_deref(),
        Some("work@example.com")
    );
    assert_eq!(
        account_ui.status_account_display,
        Some(StatusAccountDisplay::ChatGptPool {
            pool_id: "codex-pro".to_string(),
            active_member: Some(StatusAccountPoolMemberDisplay {
                id: "work-pro".to_string(),
                email: Some("work@example.com".to_string()),
                plan: Some("Pro".to_string()),
            }),
            member_count: 2,
            unavailable_count: 1,
        })
    );
    assert_eq!(
        account_ui.plan_type,
        Some(codex_protocol::account::PlanType::Pro)
    );
    assert_eq!(account_ui.has_chatgpt_account, true);
}

#[test]
fn account_ui_state_from_response_uses_pool_member_metadata_without_active_assignment() {
    let response = GetAccountResponse {
        account: Some(Account::ChatgptPool {
            id: "codex-pro".to_string(),
            active_account_id: None,
            members: vec![
                codex_app_server_protocol::AccountPoolMember {
                    id: "work-pro".to_string(),
                    email: Some("work@example.com".to_string()),
                    plan_type: Some(codex_protocol::account::PlanType::Pro),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: Some(80),
                    spark_remaining: Some(50),
                    last_error: None,
                },
                codex_app_server_protocol::AccountPoolMember {
                    id: "personal-pro".to_string(),
                    email: Some("personal@example.com".to_string()),
                    plan_type: Some(codex_protocol::account::PlanType::Plus),
                    active: false,
                    unavailable_reason: None,
                    regular_remaining: Some(60),
                    spark_remaining: Some(40),
                    last_error: None,
                },
            ],
        }),
        requires_openai_auth: true,
    };

    let account_ui = account_ui_state_from_response(&response);

    assert_eq!(
        account_ui.account_email.as_deref(),
        Some("work@example.com")
    );
    assert_eq!(
        account_ui.status_account_display,
        Some(StatusAccountDisplay::ChatGptPool {
            pool_id: "codex-pro".to_string(),
            active_member: None,
            member_count: 2,
            unavailable_count: 0,
        })
    );
    assert_eq!(
        account_ui.plan_type,
        Some(codex_protocol::account::PlanType::Pro)
    );
    assert_eq!(account_ui.has_chatgpt_account, true);
}
