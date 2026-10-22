use super::ResponsesStreamRequest;
use super::ResponsesStreamRetryState;
use super::handle_response_stream_error;
use super::log_retry;
use crate::realtime_history::RealtimeHistoryState;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing_test::internal::MockWriter;

#[tokio::test]
async fn capacity_retries_back_off_indefinitely_without_consuming_stream_budget() {
    let (session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    let mut retry_state = ResponsesStreamRetryState::default();
    tokio::time::pause();

    for seconds in [10, 20, 40, 80, 160, 300, 300, 300] {
        let start = Instant::now();
        let retry = handle_response_stream_error(
            &mut retry_state,
            /*max_retries*/ 0,
            CodexErr::ServerOverloaded,
            &mut client_session,
            &session,
            &step_context,
            ResponsesStreamRequest::Sampling,
        );
        tokio::pin!(retry);
        assert!(futures::poll!(&mut retry).is_pending());
        tokio::time::advance(Duration::from_secs(seconds - 1)).await;
        assert!(futures::poll!(&mut retry).is_pending());
        retry.await.expect("capacity retry should remain allowed");
        let elapsed = Instant::now() - start;
        let expected = Duration::from_secs(seconds);
        assert!((expected..=expected + Duration::from_millis(/*millis*/ 1)).contains(&elapsed));
    }
    assert_eq!((retry_state.retries, retry_state.capacity_retries), (0, 8));
}

#[tokio::test]
async fn capacity_retry_advice_stays_within_backoff_bounds() {
    let (session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    let mut client_session = session.services.model_client.new_session();
    tokio::time::pause();
    for (advised_seconds, expected_seconds) in [(0, 10), (1, 10), (30, 30), (600, 300)] {
        let mut retry_state = ResponsesStreamRetryState::default();
        let start = Instant::now();
        let advice = RetryAfter::from_delay(Duration::from_secs(advised_seconds))
            .expect("valid retry deadline");
        handle_response_stream_error(
            &mut retry_state,
            /*max_retries*/ 0,
            CodexErr::ServerOverloaded.with_retry_after(advice),
            &mut client_session,
            &session,
            &step_context,
            ResponsesStreamRequest::RemoteCompactionV2,
        )
        .await
        .expect("capacity retry should remain allowed");
        let elapsed = Instant::now() - start;
        // Tokio rounds timer deadlines up to the next millisecond.
        let expected = Duration::from_secs(expected_seconds);
        assert!((expected..=expected + Duration::from_millis(/*millis*/ 1)).contains(&elapsed));
    }
}

#[tokio::test]
async fn sampling_retry_logs_stream_error_context() {
    let (_session, turn_context) = make_session_and_context().await;
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    log_retry(
        ResponsesStreamRequest::Sampling,
        &turn_context,
        &CodexErr::Stream("websocket closed by server before response.completed".to_string()),
        /*retries*/ 2,
        /*max_retries*/ 5,
        Duration::from_secs(1),
    );

    let logs = String::from_utf8(
        buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("retry log should be valid utf-8");
    assert!(logs.contains("stream disconnected - retrying sampling request"));
    assert!(logs.contains(&format!("turn_id={}", turn_context.sub_id)));
    assert!(logs.contains("retries=2"));
    assert!(logs.contains("max_retries=5"));
    assert!(logs.contains(
        "sampling_error=stream disconnected before completion: websocket closed by server before response.completed"
    ));
}

/// Time spent reporting a retry must count toward the server's original deadline.
#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test holds the event-delivery lock to delay notification while virtual time advances"
)]
async fn stream_retry_preserves_deadline_across_delayed_notification() {
    let (mut session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    session.realtime_history = Some(Mutex::new(RealtimeHistoryState::default()));
    let mut client_session = session.services.model_client.new_session();
    // The second retry emits a notification even when release builds hide the first.
    let mut retry_state = ResponsesStreamRetryState {
        retries: 1,
        ..Default::default()
    };

    tokio::time::pause();
    let advice = RetryAfter::from_delay(Duration::from_secs(10)).expect("retry deadline");
    tokio::time::advance(Duration::from_secs(4)).await;

    // Hold the event-delivery lock so reporting the error consumes part of the deadline.
    let history_guard = session.realtime_history.as_ref().unwrap().lock().await;
    let retry = handle_response_stream_error(
        &mut retry_state,
        /*max_retries*/ 2,
        CodexErr::InternalServerError.with_retry_after(advice),
        &mut client_session,
        &session,
        &step_context,
        ResponsesStreamRequest::Sampling,
    );
    tokio::pin!(retry);
    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(4)).await;
    drop(history_guard);

    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(futures::poll!(&mut retry).is_pending());
    retry.await.expect("retry should be allowed");
    // Tokio rounds timer deadlines up to the next millisecond.
    let resumed_at = Instant::now();
    assert!(
        (advice.deadline()..=advice.deadline() + Duration::from_millis(1)).contains(&resumed_at),
        "retry resumed at {resumed_at:?}, expected {advice:?}"
    );
}
