use anyhow::Result;
use app_test_support::TestAppServer;
use app_test_support::create_fake_rollout_with_token_usage;
use app_test_support::rollout_path;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_protocol::ThreadId;
use codex_protocol::items::ContextCompactionItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::ContextCompactedEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ItemCompletedEvent;
use codex_protocol::protocol::TurnStartedEvent;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
async fn resume_replays_usage_with_its_completed_compaction_item_id() -> Result<()> {
    let codex_home = TempDir::new()?;
    let filename_ts = "2025-01-05T12-00-00";
    let meta_rfc3339 = "2025-01-05T12:00:00Z";
    let conversation_id = create_fake_rollout_with_token_usage(
        codex_home.path(),
        filename_ts,
        meta_rfc3339,
        "Saved user message",
        /*model_provider*/ None,
    )?;
    let path = rollout_path(codex_home.path(), filename_ts, &conversation_id);
    let mut lines = std::fs::read_to_string(&path)?
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let token_count = lines.pop().expect("usage event");
    let thread_id = ThreadId::from_string(&conversation_id)?;
    let turn_id = "compaction-turn";
    let item = TurnItem::ContextCompaction(ContextCompactionItem {
        id: "compact-stable".to_string(),
    });
    let event_line = |event: EventMsg| -> Result<String> {
        Ok(json!({
            "timestamp": meta_rfc3339,
            "type": "event_msg",
            "payload": serde_json::to_value(event)?,
        })
        .to_string())
    };
    lines.extend([
        event_line(EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: turn_id.to_string(),
            root_turn_id: None,
            trace_id: None,
            started_at: None,
            model_context_window: None,
            collaboration_mode_kind: Default::default(),
        }))?,
        token_count,
        event_line(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id,
            turn_id: turn_id.to_string(),
            item,
            started_at_ms: Some(1),
            completed_at_ms: 2,
        }))?,
        event_line(EventMsg::ContextCompacted(ContextCompactedEvent))?,
    ]);
    std::fs::write(&path, format!("{}\n", lines.join("\n")))?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized()
        .await?;
    let resume_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: conversation_id,
            ..Default::default()
        })
        .await?;
    let ThreadResumeResponse { thread, .. } = timeout(
        Duration::from_secs(/*secs*/ 25),
        app_server.read_response(resume_id),
    )
    .await??;
    let notification = timeout(
        Duration::from_secs(/*secs*/ 25),
        app_server.read_stream_until_notification_message("thread/tokenUsage/updated"),
    )
    .await??;
    let ServerNotification::ThreadTokenUsageUpdated(notification) = notification.try_into()? else {
        panic!("expected thread/tokenUsage/updated");
    };
    assert_eq!(notification.turn_id, turn_id);
    assert_eq!(
        notification.usage_after_compaction_item_id,
        Some("compact-stable".to_string())
    );
    assert_eq!(
        thread.turns.last().expect("compaction turn").items,
        vec![ThreadItem::ContextCompaction {
            id: "compact-stable".to_string()
        }]
    );
    Ok(())
}
