use super::*;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnItemContributor;
use codex_protocol::items::AgentMessageContent;
use pretty_assertions::assert_eq;
use std::sync::Arc;

struct RewriteAgentMessageContributor;

#[async_trait::async_trait]
impl TurnItemContributor for RewriteAgentMessageContributor {
    async fn contribute(
        &self,
        _thread_store: &ExtensionData,
        _turn_store: &ExtensionData,
        item: &mut TurnItem,
    ) -> Result<(), String> {
        if let TurnItem::AgentMessage(agent_message) = item {
            agent_message.content = vec![AgentMessageContent::Text {
                text: "plan contributed assistant text".to_string(),
            }];
        }
        Ok(())
    }
}

fn assistant_output_text(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some("msg-1".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

#[tokio::test]
async fn plan_mode_uses_contributed_turn_item_for_last_agent_message() {
    let (mut session, turn_context) = crate::session::tests::make_session_and_context().await;
    let mut builder = codex_extension_api::ExtensionRegistryBuilder::new();
    builder.turn_item_contributor(Arc::new(RewriteAgentMessageContributor));
    session.services.extensions = Arc::new(builder.build());
    let turn_store = ExtensionData::new(turn_context.sub_id.clone());
    let mut state = PlanModeStreamState::new(&turn_context.sub_id);
    let mut last_agent_message = None;
    let item = assistant_output_text("original assistant text");

    let handled = handle_assistant_item_done_in_plan_mode(
        &session,
        &turn_context,
        &turn_store,
        &item,
        &mut state,
        /*previously_active_item*/ None,
        &mut last_agent_message,
    )
    .await;

    assert!(handled);
    assert_eq!(
        last_agent_message.as_deref(),
        Some("plan contributed assistant text")
    );
}

#[test]
fn usage_limit_switches_account_for_exhausted_five_hour_window() {
    let error = usage_limit_error(Some(rate_limit_window(
        /*used_percent*/ 100.0,
        FIVE_HOUR_LIMIT_WINDOW_MINUTES,
    )));

    assert!(usage_limit_should_switch_account(&error));
}

#[test]
fn usage_limit_switches_account_for_exhausted_weekly_window() {
    let error = UsageLimitReachedError {
        rate_limits: Some(Box::new(RateLimitSnapshot {
            secondary: Some(rate_limit_window(
                /*used_percent*/ 100.0,
                WEEKLY_LIMIT_WINDOW_MINUTES,
            )),
            ..rate_limit_snapshot()
        })),
        ..usage_limit_error(/*primary*/ None)
    };

    assert!(usage_limit_should_switch_account(&error));
}

#[test]
fn usage_limit_does_not_switch_account_for_short_window() {
    let error = usage_limit_error(Some(rate_limit_window(
        /*used_percent*/ 100.0, /*window_minutes*/ 15,
    )));

    assert!(!usage_limit_should_switch_account(&error));
}

#[test]
fn usage_limit_does_not_switch_account_before_window_is_exhausted() {
    let error = usage_limit_error(Some(rate_limit_window(
        /*used_percent*/ 99.9,
        FIVE_HOUR_LIMIT_WINDOW_MINUTES,
    )));

    assert!(!usage_limit_should_switch_account(&error));
}

fn usage_limit_error(primary: Option<RateLimitWindow>) -> UsageLimitReachedError {
    UsageLimitReachedError {
        plan_type: None,
        resets_at: None,
        rate_limits: Some(Box::new(RateLimitSnapshot {
            primary,
            ..rate_limit_snapshot()
        })),
        promo_message: None,
        rate_limit_reached_type: None,
    }
}

fn rate_limit_snapshot() -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

fn rate_limit_window(used_percent: f64, window_minutes: i64) -> RateLimitWindow {
    RateLimitWindow {
        used_percent,
        window_minutes: Some(window_minutes),
        resets_at: None,
    }
}
