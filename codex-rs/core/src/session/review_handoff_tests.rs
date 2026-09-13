use codex_protocol::ThreadId;
use codex_protocol::items::ExitedReviewModeItem;
use codex_protocol::models::ContentItem;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ThreadRolledBackEvent;
use pretty_assertions::assert_eq;

use super::*;

fn completed_report(item_id: &str) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
        thread_id: ThreadId::new(),
        turn_id: format!("turn-{item_id}"),
        item: TurnItem::ExitedReviewMode(ExitedReviewModeItem {
            id: item_id.to_string(),
            review_output: Some(ReviewOutputEvent {
                overall_explanation: item_id.to_string(),
                ..Default::default()
            }),
        }),
        completed_at_ms: 0,
    }))
}

#[test]
fn reconstruction_keeps_only_reports_after_the_latest_consumed_handoff() {
    let first = PendingReviewReport {
        item_id: "first".to_string(),
        output: ReviewOutputEvent::default(),
    };
    let consumed = ReviewHandoff::consumption_marker(&first.item_id);
    let mut rollout = vec![completed_report("first")];
    rollout.push(RolloutItem::ResponseItem(consumed));
    rollout.push(completed_report("second"));

    let pending = pending_review_reports_from_rollout(&rollout);
    assert_eq!(
        pending
            .iter()
            .map(|report| report.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["second"]
    );
}

#[test]
fn reconstruction_restores_a_durable_pending_report_marker() {
    let report = PendingReviewReport::new(
        "durable".to_string(),
        ReviewOutputEvent {
            overall_explanation: "persisted".to_string(),
            ..Default::default()
        },
    );
    let rollout = vec![RolloutItem::ResponseItem(
        ReviewHandoff::pending_report_marker(&report),
    )];

    let pending = pending_review_reports_from_rollout(&rollout);

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_id, "durable");
    assert_eq!(pending[0].output.overall_explanation, "persisted");
}

#[test]
fn accepted_user_boundary_consumes_a_persisted_handoff_without_a_marker() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let mut rollout = vec![completed_report("review")];
    rollout.extend(
        ReviewHandoff::new(&[report])
            .expect("handoff")
            .into_response_items()
            .into_iter()
            .map(RolloutItem::ResponseItem),
    );
    rollout.push(user_message("accepted input"));

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn rollback_restores_a_surviving_report_whose_consuming_turn_was_removed() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let mut rollout = vec![completed_report("review")];
    rollout.extend(
        ReviewHandoff::new(&[report])
            .expect("handoff")
            .into_response_items()
            .into_iter()
            .map(RolloutItem::ResponseItem),
    );
    rollout.extend([
        user_message("consume report"),
        RolloutItem::ResponseItem(ReviewHandoff::consumption_marker("review")),
        rollback(/*num_turns*/ 1),
    ]);

    let pending = pending_review_reports_from_rollout(&rollout);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_id, "review");
}

#[test]
fn rollback_drops_a_review_completed_after_the_removed_turn_started() {
    let rollout = vec![
        user_message("start turn"),
        completed_report("discarded"),
        rollback(/*num_turns*/ 1),
    ];

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn reconstruction_deduplicates_item_and_legacy_review_events() {
    let output = ReviewOutputEvent {
        overall_explanation: "same report".to_string(),
        ..Default::default()
    };
    let rollout = vec![
        completed_report("review"),
        RolloutItem::EventMsg(EventMsg::ExitedReviewMode(
            codex_protocol::protocol::ExitedReviewModeEvent {
                turn_id: Some("turn-review".to_string()),
                item_id: Some("review".to_string()),
                review_output: Some(output),
            },
        )),
    ];

    assert_eq!(pending_review_reports_from_rollout(&rollout).len(), 1);
}

#[test]
fn legacy_model_visible_review_messages_mark_old_reports_consumed() {
    let rollout = vec![
        completed_report("old"),
        RolloutItem::ResponseItem(ResponseItem::Message {
            id: Some(LEGACY_REVIEW_ASSISTANT_MESSAGE_ID.to_string()),
            role: "assistant".to_string(),
            content: Vec::new(),
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }),
    ];

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

fn user_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: Some(format!("user-{text}")),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    })
}

fn rollback(num_turns: u32) -> RolloutItem {
    RolloutItem::EventMsg(EventMsg::ThreadRolledBack(ThreadRolledBackEvent {
        num_turns,
    }))
}
