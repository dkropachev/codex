use super::*;
use codex_protocol::ThreadId;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use pretty_assertions::assert_eq;

#[test]
fn modern_compaction_ids_survive_history_reconstruction() {
    let thread_id = ThreadId::new();
    let turn_id = "turn-1".to_string();
    let mut builder = ThreadHistoryBuilder::new();
    builder.handle_event(&EventMsg::TurnStarted(TurnStartedEvent {
        turn_id: turn_id.clone(),
        root_turn_id: None,
        trace_id: None,
        started_at: None,
        model_context_window: None,
        collaboration_mode_kind: Default::default(),
    }));
    for id in ["compact-1", "compact-2"] {
        let item = TurnItem::ContextCompaction(ContextCompactionItem { id: id.to_string() });
        builder.handle_event(&EventMsg::ItemStarted(ItemStartedEvent {
            thread_id,
            turn_id: turn_id.clone(),
            item: item.clone(),
            started_at_ms: 1,
        }));
        builder.handle_event(&EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.clone(),
            item,
            started_at_ms: Some(1),
            completed_at_ms: 2,
        }));
        builder.handle_event(&EventMsg::ContextCompacted(ContextCompactedEvent));
    }
    assert_eq!(
        builder.finish()[0].items,
        vec![
            ThreadItem::ContextCompaction {
                id: "compact-1".to_string()
            },
            ThreadItem::ContextCompaction {
                id: "compact-2".to_string()
            },
        ]
    );

    let mut legacy = ThreadHistoryBuilder::new();
    legacy.handle_event(&EventMsg::ContextCompacted(ContextCompactedEvent));
    legacy.handle_event(&EventMsg::ContextCompacted(ContextCompactedEvent));
    assert_eq!(
        legacy.finish()[0].items,
        vec![
            ThreadItem::ContextCompaction {
                id: "item-1".to_string()
            },
            ThreadItem::ContextCompaction {
                id: "item-2".to_string()
            },
        ]
    );
}
