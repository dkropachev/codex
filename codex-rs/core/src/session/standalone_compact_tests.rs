use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use codex_protocol::AgentPath;
use codex_protocol::protocol::InterAgentCommunication;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn idle_compaction_rejects_pending_trigger_mail_without_consuming_it() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let communication = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").expect("valid agent path"),
        AgentPath::root(),
        Vec::new(),
        "mail before compaction".to_string(),
        /*trigger_turn*/ true,
    );
    session
        .input_queue
        .enqueue_mailbox_communication(communication, Default::default())
        .await;

    assert_eq!(
        run_if_idle(
            &session,
            "compact-turn".to_string(),
            CompactionSource::Automatic
        )
        .await,
        StartIfIdleSubmission::NotSubmitted {
            reason: NotSubmittedReason::PendingTriggerTurn,
        }
    );
    assert!(session.active_turn.lock().await.is_none());
    assert!(session.input_queue.has_trigger_turn_mailbox_items().await);
}
