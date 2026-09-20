use crate::outgoing_message::ThreadScopedOutgoingMessageSender;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::RawResponseItemCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::review_format::format_review_findings_block;

const LEGACY_REVIEW_USER_ID: &str = "review_rollout_user";
const LEGACY_REVIEW_ASSISTANT_ID: &str = "review_rollout_assistant";

pub(crate) async fn emit_legacy_review_user_raw_item(
    conversation_id: ThreadId,
    turn_id: &str,
    output: Option<&ReviewOutputEvent>,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    outgoing
        .send_server_notification(ServerNotification::RawResponseItemCompleted(
            RawResponseItemCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: turn_id.to_string(),
                item: ResponseItem::Message {
                    id: Some(LEGACY_REVIEW_USER_ID.to_string()),
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: legacy_review_user_message(output),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                },
            },
        ))
        .await;
}

pub(crate) async fn emit_legacy_review_agent_message(
    conversation_id: ThreadId,
    turn_id: String,
    review: String,
    timestamp_ms: i64,
    outgoing: &ThreadScopedOutgoingMessageSender,
) {
    outgoing
        .send_server_notification(ServerNotification::RawResponseItemCompleted(
            RawResponseItemCompletedNotification {
                thread_id: conversation_id.to_string(),
                turn_id: turn_id.clone(),
                item: ResponseItem::Message {
                    id: Some(LEGACY_REVIEW_ASSISTANT_ID.to_string()),
                    role: "assistant".to_string(),
                    content: vec![ContentItem::OutputText {
                        text: review.clone(),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: None,
                },
            },
        ))
        .await;
    let item = ThreadItem::AgentMessage {
        id: LEGACY_REVIEW_ASSISTANT_ID.to_string(),
        text: review,
        phase: None,
        memory_citation: None,
    };
    outgoing
        .send_server_notification(ServerNotification::ItemStarted(ItemStartedNotification {
            item: item.clone(),
            thread_id: conversation_id.to_string(),
            turn_id: turn_id.clone(),
            started_at_ms: timestamp_ms,
        }))
        .await;
    outgoing
        .send_server_notification(ServerNotification::ItemCompleted(
            ItemCompletedNotification {
                item,
                thread_id: conversation_id.to_string(),
                turn_id,
                completed_at_ms: timestamp_ms,
            },
        ))
        .await;
}

fn legacy_review_user_message(output: Option<&ReviewOutputEvent>) -> String {
    let Some(output) = output else {
        return "<user_action>\n  <context>User initiated a review task, but was interrupted. If user asks about this, tell them to re-initiate a review with `/review` and wait for it to complete.</context>\n  <action>review</action>\n  <results>\n  None.\n  </results>\n</user_action>\n".to_string();
    };
    let mut results = output.overall_explanation.trim().to_string();
    if !output.findings.is_empty() {
        let findings = format_review_findings_block(&output.findings, /*selection*/ None);
        results.push_str(&format!("\n{findings}"));
    }
    format!(
        "<user_action>\n  <context>User initiated a review task. Here's the full review output from reviewer model. User may select one or more comments to resolve.</context>\n  <action>review</action>\n  <results>\n  {results}\n  </results>\n  </user_action>\n"
    )
}
