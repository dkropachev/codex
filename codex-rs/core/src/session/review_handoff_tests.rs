use codex_protocol::ThreadId;
use codex_protocol::items::EnteredReviewModeItem;
use codex_protocol::items::ExitedReviewModeItem;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::protocol::EnteredReviewModeEvent;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::ReviewOutputEvent;
use codex_protocol::protocol::ReviewTarget;
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

fn entered_review(item_id: &str) -> [RolloutItem; 2] {
    let target = ReviewTarget::WholeRepository;
    [
        RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: ThreadId::new(),
            turn_id: format!("turn-{item_id}"),
            item: TurnItem::EnteredReviewMode(EnteredReviewModeItem {
                id: item_id.to_string(),
                target: target.clone(),
                user_facing_hint: "whole repository".to_string(),
            }),
            completed_at_ms: 0,
        })),
        RolloutItem::EventMsg(EventMsg::EnteredReviewMode(EnteredReviewModeEvent {
            target,
            user_facing_hint: Some("whole repository".to_string()),
            turn_id: Some(format!("turn-{item_id}")),
            item_id: Some(item_id.to_string()),
        })),
    ]
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
fn duplicate_old_consumption_does_not_remove_a_newer_report() {
    let rollout = vec![
        completed_report("first"),
        RolloutItem::ResponseItem(ReviewHandoff::consumption_marker("first")),
        completed_report("second"),
        RolloutItem::ResponseItem(ReviewHandoff::consumption_marker("first")),
    ];

    let pending = pending_review_reports_from_rollout(&rollout);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].item_id, "second");
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
    let mut items = ReviewHandoff::new(&[report])
        .expect("handoff")
        .into_response_items();
    for (index, item) in items.iter_mut().enumerate() {
        if let ResponseItem::Message { id, .. } = item {
            *id = Some(format!("review_handoff_part:review:{index}"));
        }
    }
    rollout.extend(items.into_iter().map(RolloutItem::ResponseItem));
    rollout.push(user_message("accepted input"));

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn incomplete_transaction_does_not_consume_a_pending_report() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let mut rollout = vec![completed_report("review")];
    let handoff = ReviewHandoff::new(&[report]).expect("handoff");
    let transaction_id = handoff.transaction_id().to_string();
    rollout.push(RolloutItem::ResponseItem(
        ReviewHandoff::transaction_marker(&transaction_id),
    ));
    rollout.extend(
        handoff
            .into_response_items()
            .into_iter()
            .map(RolloutItem::ResponseItem),
    );
    rollout.push(user_message("accepted input"));

    assert_eq!(pending_review_reports_from_rollout(&rollout).len(), 1);
    rollout.push(RolloutItem::ResponseItem(
        ReviewHandoff::committed_transaction_marker(&transaction_id, "review"),
    ));
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
fn rollback_counts_a_fragmented_handoff_as_part_of_one_user_turn() {
    let reports = (0..65)
        .map(|index| {
            PendingReviewReport::new(
                format!("review-{index}"),
                ReviewOutputEvent {
                    overall_explanation: format!("report {index} {}", "x".repeat(200)),
                    ..Default::default()
                },
            )
        })
        .collect::<Vec<_>>();
    let handoff = ReviewHandoff::new(&reports).expect("handoff");
    let transaction_id = handoff.transaction_id().to_string();
    let mut handoff_items = handoff.into_response_items();
    assert!(handoff_items.len() > 1);

    let mut rollout = (0..65)
        .map(|index| completed_report(&format!("review-{index}")))
        .collect::<Vec<_>>();
    rollout.push(RolloutItem::ResponseItem(
        ReviewHandoff::transaction_marker(&transaction_id),
    ));
    rollout.extend(handoff_items.drain(..).map(RolloutItem::ResponseItem));
    rollout.extend([
        user_message("consume report"),
        RolloutItem::ResponseItem(ReviewHandoff::committed_transaction_marker(
            &transaction_id,
            "review-64",
        )),
        user_message("second turn"),
        rollback(/*num_turns*/ 2),
    ]);

    let pending = pending_review_reports_from_rollout(&rollout);
    assert_eq!(pending.len(), 65);
    assert_eq!(pending[0].item_id, "review-0");
    assert_eq!(pending[64].item_id, "review-64");
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
fn rollback_counts_modern_inter_agent_input_once() {
    let rollout = vec![
        user_message("first"),
        completed_report("after-first"),
        RolloutItem::InterAgentCommunicationMetadata { trigger_turn: true },
        RolloutItem::ResponseItem(ResponseItem::AgentMessage {
            id: Some("communication".to_string()),
            author: "/root/worker".to_string(),
            recipient: "/root".to_string(),
            content: vec![AgentMessageInputContent::InputText {
                text: "mail".to_string(),
            }],
            internal_chat_message_metadata_passthrough: None,
        }),
        user_message("second"),
        rollback(/*num_turns*/ 3),
    ];

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn rollback_counts_paired_review_events_once() {
    let mut rollout = Vec::new();
    rollout.extend(entered_review("first"));
    rollout.push(completed_report("first"));
    rollout.extend(entered_review("second"));
    rollout.push(completed_report("second"));
    rollout.push(rollback(/*num_turns*/ 1));

    let pending = pending_review_reports_from_rollout(&rollout);
    assert_eq!(
        pending
            .iter()
            .map(|report| report.item_id.as_str())
            .collect::<Vec<_>>(),
        vec!["first"]
    );

    rollout.push(rollback(/*num_turns*/ 1));
    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn rollback_of_a_steer_keeps_the_original_handoff_consumed() {
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
        user_message("original prompt"),
        RolloutItem::ResponseItem(ReviewHandoff::consumption_marker("review")),
        user_message("steer"),
        rollback(/*num_turns*/ 1),
    ]);

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn legacy_review_wrapper_and_entered_event_count_as_one_rollback_turn() {
    let mut rollout = vec![user_message("prior turn"), completed_report("prior")];
    rollout.extend(entered_review("review"));
    rollout.extend([
        legacy_review_user_message("legacy report"),
        completed_report("review"),
        rollback(/*num_turns*/ 2),
    ]);

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());
}

#[test]
fn partial_legacy_exit_does_not_reinject_an_already_visible_report() {
    let output = ReviewOutputEvent {
        overall_explanation: "already visible".to_string(),
        ..Default::default()
    };
    let rollout = vec![
        legacy_review_user_message("legacy report"),
        RolloutItem::EventMsg(EventMsg::ExitedReviewMode(
            codex_protocol::protocol::ExitedReviewModeEvent {
                turn_id: Some("review-turn".to_string()),
                item_id: Some("review-item".to_string()),
                review_output: Some(output),
            },
        )),
    ];

    assert!(pending_review_reports_from_rollout(&rollout).is_empty());

    let mut followed_by_new_review = rollout;
    followed_by_new_review.push(completed_report("new-review"));
    assert_eq!(
        pending_review_reports_from_rollout(&followed_by_new_review)[0].item_id,
        "new-review"
    );

    let interrupted_then_new_review = vec![
        legacy_review_user_message("legacy report"),
        RolloutItem::EventMsg(EventMsg::ExitedReviewMode(
            codex_protocol::protocol::ExitedReviewModeEvent {
                turn_id: Some("interrupted".to_string()),
                item_id: Some("interrupted".to_string()),
                review_output: None,
            },
        )),
        completed_report("after-interruption"),
    ];
    assert_eq!(
        pending_review_reports_from_rollout(&interrupted_then_new_review)[0].item_id,
        "after-interruption"
    );
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

#[test]
fn history_filter_drops_an_incomplete_handoff_batch() {
    let mut filter = ReviewHandoffHistoryFilter::new();
    let handoff = ReviewHandoff::new(&[PendingReviewReport::new(
        "review".to_string(),
        ReviewOutputEvent::default(),
    )])
    .expect("handoff")
    .into_response_items()
    .remove(0);
    assert!(filter.push(handoff).is_empty());

    let assistant = ResponseItem::Message {
        id: Some("assistant".to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "answer".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert_eq!(filter.push(assistant.clone()), vec![assistant]);
}

#[test]
fn history_filter_keeps_a_handoff_followed_by_accepted_input() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let mut items = ReviewHandoff::new(&[report])
        .expect("handoff")
        .into_response_items();
    let user = match user_message("accepted") {
        RolloutItem::ResponseItem(user) => user,
        _ => unreachable!("user_message always returns a response item"),
    };
    items.push(user);

    assert_eq!(filter_complete_review_handoff_history(items.clone()), items);
}

#[test]
fn history_filter_commits_new_handoffs_only_at_the_transaction_marker() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let handoff = ReviewHandoff::new(&[report]).expect("handoff");
    let transaction_id = handoff.transaction_id().to_string();
    let handoff = handoff.into_response_items();
    let user = match user_message("accepted") {
        RolloutItem::ResponseItem(user) => user,
        _ => unreachable!("user_message always returns a response item"),
    };
    let mut transaction = vec![ReviewHandoff::transaction_marker(&transaction_id)];
    transaction.extend(handoff.clone());
    transaction.push(user.clone());

    assert!(filter_complete_review_handoff_history(transaction.clone()).is_empty());
    transaction.push(ReviewHandoff::committed_transaction_marker(
        &transaction_id,
        "review",
    ));
    let mut expected = handoff;
    expected.push(user);
    assert_eq!(
        filter_complete_review_handoff_history(transaction),
        expected
    );
}

#[test]
fn history_filter_deduplicates_retried_handoff_transactions() {
    let report = PendingReviewReport::new("review".to_string(), ReviewOutputEvent::default());
    let handoff = ReviewHandoff::new(&[report]).expect("handoff");
    let transaction_id = handoff.transaction_id().to_string();
    let handoff = handoff.into_response_items();
    let user = match user_message("accepted") {
        RolloutItem::ResponseItem(user) => user,
        _ => unreachable!("user_message always returns a response item"),
    };
    let mut one = vec![ReviewHandoff::transaction_marker(&transaction_id)];
    one.extend(handoff.clone());
    one.push(user.clone());
    one.push(ReviewHandoff::committed_transaction_marker(
        &transaction_id,
        "review",
    ));
    let mut retried = one.clone();
    retried.extend(one);

    let mut expected = handoff;
    expected.push(user);
    assert_eq!(filter_complete_review_handoff_history(retried), expected);
}

#[test]
fn history_filter_allows_a_new_delivery_after_rollback() {
    let mut filter = ReviewHandoffHistoryFilter::new();
    for transaction_id in ["delivery-one", "delivery-two"] {
        let handoff = ResponseItem::Message {
            id: Some(format!("review_handoff_part:{transaction_id}:0")),
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "<review_handoff>report</review_handoff>".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let user = match user_message(transaction_id) {
            RolloutItem::ResponseItem(user) => user,
            _ => unreachable!("user_message always returns a response item"),
        };
        assert!(
            filter
                .push(ReviewHandoff::transaction_marker(transaction_id))
                .is_empty()
        );
        assert!(filter.push(handoff).is_empty());
        assert!(filter.push(user).is_empty());
        assert_eq!(
            filter
                .push(ReviewHandoff::committed_transaction_marker(
                    transaction_id,
                    "review",
                ))
                .len(),
            2
        );
        filter.interrupt();
    }
}

#[test]
fn history_filter_rejects_oversized_persisted_handoff_fragments() {
    let oversized = ResponseItem::Message {
        id: Some("review_handoff_part:transaction:0".to_string()),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: format!(
                "<review_handoff>{}</review_handoff>",
                "x".repeat(crate::context::MAX_HANDOFF_FRAGMENT_BYTES)
            ),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let user = match user_message("accepted") {
        RolloutItem::ResponseItem(user) => user,
        _ => unreachable!("user_message always returns a response item"),
    };
    let items = vec![
        ReviewHandoff::transaction_marker("transaction"),
        oversized,
        user,
        ReviewHandoff::committed_transaction_marker("transaction", "review"),
    ];

    assert!(filter_complete_review_handoff_history(items).is_empty());
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

fn legacy_review_user_message(text: &str) -> RolloutItem {
    RolloutItem::ResponseItem(ResponseItem::Message {
        id: Some(LEGACY_REVIEW_USER_MESSAGE_ID.to_string()),
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
