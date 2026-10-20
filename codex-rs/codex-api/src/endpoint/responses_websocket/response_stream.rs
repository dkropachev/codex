//! Owns each response's exclusive socket lease, interruption deadline and acknowledgement.
//! Completion survives cancellation until usage can be delivered after the lease is released.

use super::*;
use crate::common::ResponseEvent;
use crate::common::ResponseInterrupt;
use crate::common::ResponseStream;
use crate::common::SafetyBufferingTreatment;
use crate::common::WS_REQUEST_HEADER_TRACEPARENT_CLIENT_METADATA_KEY;
use crate::rate_limits::parse_rate_limit_event;
use crate::safety_buffering::treatment_from_headers;
use crate::sse::ResponsesStreamEvent;
use crate::sse::process_responses_event;
use futures::FutureExt;
use std::future::Future;
use tokio::time::Instant;
use tracing::Instrument;
use tracing::Span;
use tracing::debug;
use tracing::instrument;

impl ResponsesWebsocketConnection {
    #[instrument(
        name = "responses_websocket.stream_request",
        target = "codex_api::endpoint::responses_websocket",
        level = "info",
        skip_all,
        fields(transport = "responses_websocket", api.path = "/responses")
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesWsRequest<'_>,
        connection_reused: bool,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let (tx_event, rx_event) =
            mpsc::channel::<std::result::Result<ResponseEvent, ApiError>>(1600);
        let (tx_interrupt, rx_interrupt) = oneshot::channel();
        let stream = Arc::clone(&self.stream);
        let idle_timeout = self.idle_timeout;
        let server_reasoning_included = self.server_reasoning_included;
        let server_model = self.server_model.clone();
        let telemetry = self.telemetry.clone();
        let ResponsesWsRequest::ResponseCreate(ws_request) = &request;
        let client_metadata = ws_request.client_metadata.as_ref();
        let timing_log_context = ResponsesWebsocketTimingLogContext {
            model: ws_request.model.to_string(),
            session_id: client_metadata
                .and_then(|metadata| metadata.get(SESSION_ID_CLIENT_METADATA_KEY))
                .cloned(),
            thread_id: client_metadata
                .and_then(|metadata| metadata.get(THREAD_ID_CLIENT_METADATA_KEY))
                .cloned(),
            turn_id: client_metadata
                .and_then(|metadata| metadata.get(TURN_ID_CLIENT_METADATA_KEY))
                .cloned(),
            traceparent: client_metadata
                .and_then(|metadata| {
                    metadata.get(WS_REQUEST_HEADER_TRACEPARENT_CLIENT_METADATA_KEY)
                })
                .cloned(),
            previous_response_id: ws_request.previous_response_id.clone(),
            request_start_ms: client_metadata
                .and_then(|metadata| metadata.get(WS_STREAM_REQUEST_START_MS_CLIENT_METADATA_KEY))
                .cloned(),
            warmup: ws_request.generate == Some(false),
            connection_reused,
        };
        let request_text = serialize_websocket_request(&request)?;

        let current_span = Span::current();
        tokio::spawn(
            #[expect(
                clippy::await_holding_invalid_type,
                reason = "the guard serializes exclusive use of the websocket stream for the lifetime of the response stream"
            )]
            async move {
                if let Some(model) = server_model {
                    let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
                }
                if server_reasoning_included {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::ServerReasoningIncluded(true)))
                        .await;
                }
                // Abandoned work has a cleanup deadline; ordinary steering preserves the response.
                let interrupt = async move {
                    match rx_interrupt.await {
                        Ok(interrupt) => interrupt,
                        Err(_) => std::future::pending().await,
                    }
                }
                .shared();
                let interrupt_timeout = async {
                    match interrupt.clone().await {
                        ResponseInterrupt::Drain => std::future::pending().await,
                        ResponseInterrupt::Cancel { deadline } => {
                            tokio::time::sleep_until(deadline).await;
                        }
                    }
                };
                tokio::pin!(interrupt_timeout);
                let mut guard = tokio::select! {
                    biased;
                    _ = tx_event.closed() => return,
                    () = &mut interrupt_timeout => {
                        let _ = tx_event.send(Err(ApiError::Stream(
                            "timed out draining interrupted response".into(),
                        ))).await;
                        return;
                    }
                    guard = stream.lock() => guard,
                };
                if tx_event.is_closed() {
                    return;
                }
                let mut completion = None;
                let result = {
                    let Some(ws_stream) = guard.as_mut() else {
                        let _ = tx_event
                            .send(Err(ApiError::Stream(
                                "websocket connection is closed".to_string(),
                            )))
                            .await;
                        return;
                    };

                    tokio::select! {
                        biased;
                        result = run_websocket_response_stream(
                            ws_stream,
                            tx_event.clone(),
                            request_text,
                            idle_timeout,
                            telemetry,
                            turn_state.as_deref(),
                            &timing_log_context,
                            interrupt.clone(),
                            &mut completion,
                        ) => result,
                        () = &mut interrupt_timeout => Err(ApiError::Stream(
                            "timed out draining interrupted response".into(),
                        )),
                        _ = tx_event.closed() => Err(ApiError::Stream(
                            "response event consumer dropped".to_string(),
                        )),
                    }
                };

                if let Err(err) = result {
                    // A terminal stream error should reach the caller immediately. Waiting for a
                    // graceful close handshake here can stall indefinitely and mask the error.
                    let failed_stream = guard.take();
                    drop(guard);
                    drop(failed_stream);
                    // Failed cancellation retires the socket while preserving any received
                    // completion and its usage for the abandoned classifier's cleanup.
                    let event = completion.map_or_else(|| Err(err), Ok);
                    let _ = tx_event.send(event).await;
                } else if let Some(completed) = completion {
                    // The handshake is finished. Consumer backpressure must not let the
                    // interruption deadline discard completion or its usage accounting.
                    drop(guard);
                    let _ = tx_event.send(Ok(completed)).await;
                }
            }
            .instrument(current_span),
        );

        Ok(ResponseStream {
            rx_event,
            upstream_request_id: None,
            interrupt: Some(tx_interrupt),
        })
    }
}

#[derive(Deserialize)]
struct InterruptAcknowledgement {
    response_id: String,
}

#[allow(clippy::too_many_arguments)]
async fn run_websocket_response_stream(
    ws_stream: &mut WsStream,
    tx_event: mpsc::Sender<std::result::Result<ResponseEvent, ApiError>>,
    request_text: String,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn WebsocketTelemetry>>,
    turn_state: Option<&OnceLock<String>>,
    timing_log_context: &ResponsesWebsocketTimingLogContext,
    interrupt: impl Future<Output = ResponseInterrupt> + Clone + Unpin,
    completion: &mut Option<ResponseEvent>,
) -> Result<(), ApiError> {
    let mut last_server_model: Option<String> = None;
    let mut latest_rate_limit_snapshot = None;
    let mut safety_buffering_treatment = SafetyBufferingTreatment::default();
    if matches!(
        interrupt.clone().now_or_never(),
        Some(ResponseInterrupt::Cancel { .. })
    ) {
        // No request reached the backend, so this socket is still reusable.
        return Ok(());
    }
    send_websocket_request(
        ws_stream,
        request_text,
        idle_timeout,
        telemetry.as_ref(),
        timing_log_context.connection_reused,
    )
    .await?;

    // A response owns its interrupt, and create must be sent before interrupt.
    let mut interrupt_request = interrupt.clone().fuse();
    let mut interruption = None;
    let mut response_id = None;
    let mut awaiting_interrupt_ack = false;
    loop {
        let poll_start = Instant::now();
        let response = tokio::select! {
            response = tokio::time::timeout(idle_timeout, ws_stream.next()) => {
                response.map_err(|_| ApiError::Stream("idle timeout waiting for websocket".into()))
            }
            request = &mut interrupt_request, if response_id.is_some() => {
                interruption = Some(request);
                send_websocket_request(
                    ws_stream,
                    serde_json::json!({
                        "type": "response.interrupt",
                        "response_id": response_id,
                        "mode": "discard_partial_items",
                    }).to_string(),
                    idle_timeout,
                    /*telemetry*/ None,
                    timing_log_context.connection_reused,
                )
                .await?;
                awaiting_interrupt_ack = matches!(request, ResponseInterrupt::Cancel { .. });
                continue;
            }
        };
        if let Some(t) = telemetry.as_ref() {
            t.on_ws_event(&response, poll_start.elapsed());
        }
        let message = match response {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(err))) => {
                return Err(map_ws_stream_error(err));
            }
            Ok(None) => {
                return Err(ApiError::Stream(
                    "stream closed before response.completed".into(),
                ));
            }
            Err(err) => {
                return Err(err);
            }
        };

        match message {
            Message::Text(text) => {
                if let Some(wrapped_error) = parse_wrapped_websocket_error_event(&text)
                    && let Some(error) =
                        map_wrapped_websocket_error_event(wrapped_error, text.to_string())
                {
                    return Err(error);
                }

                let event = match serde_json::from_str::<ResponsesStreamEvent>(&text) {
                    Ok(event) => event,
                    Err(err) => {
                        debug!(target: "codex_api::endpoint::responses_websocket", "failed to parse websocket event: {err}, data: {text}");
                        continue;
                    }
                };
                if matches!(interruption, Some(ResponseInterrupt::Cancel { .. }))
                    && matches!(
                        event.kind(),
                        "response.interrupt.accepted" | "response.interrupt.failed"
                    )
                {
                    let acknowledgement: InterruptAcknowledgement = serde_json::from_str(&text)
                        .map_err(|_| {
                            ApiError::Stream("invalid interruption acknowledgement".into())
                        })?;
                    if !awaiting_interrupt_ack
                        || response_id.as_ref() != Some(&acknowledgement.response_id)
                    {
                        return Err(ApiError::Stream(
                            "unexpected interruption acknowledgement".into(),
                        ));
                    }
                    if event.kind() == "response.interrupt.failed" {
                        return Err(ApiError::Stream(
                            "response interruption was rejected".into(),
                        ));
                    }
                    awaiting_interrupt_ack = false;
                    if completion.is_some() {
                        break;
                    }
                    continue;
                }
                emit_responses_websocket_timing_event(
                    event.kind(),
                    text.as_str(),
                    timing_log_context,
                );
                if event.kind() == "codex.response.metadata"
                    && let Some(etag) =
                        event
                            .headers
                            .as_ref()
                            .and_then(Value::as_object)
                            .and_then(|headers| {
                                json_headers_to_http_headers(headers)
                                    .get(X_MODELS_ETAG_HEADER)
                                    .and_then(|value| value.to_str().ok())
                                    .map(str::to_string)
                            })
                {
                    let _ = tx_event.send(Ok(ResponseEvent::ModelsEtag(etag))).await;
                }
                if let Some(response_turn_state) = event.turn_state()
                    && let Some(turn_state) = turn_state
                {
                    let _ = turn_state.set(response_turn_state);
                }
                let model_verifications = event.model_verifications();
                let turn_moderation_metadata = event.turn_moderation_metadata();
                let safety_buffering =
                    safety_buffering_for_event(&event, &mut safety_buffering_treatment);
                if event.kind() == "codex.rate_limits" {
                    if let Some(snapshot) = parse_rate_limit_event(&text) {
                        latest_rate_limit_snapshot = Some(snapshot.clone());
                        let _ = tx_event.send(Ok(ResponseEvent::RateLimits(snapshot))).await;
                    }
                    continue;
                }
                let model = event.response_model();
                // Preserve terminal usage before any accompanying notification can block.
                let event =
                    match process_responses_event(event, latest_rate_limit_snapshot.as_ref()) {
                        Ok(Some(event @ ResponseEvent::Completed { .. })) => {
                            *completion = Some(event);
                            Ok(None)
                        }
                        event => event,
                    };
                if let Some(model) = model
                    && last_server_model.as_deref() != Some(model.as_str())
                {
                    let _ = tx_event
                        .send(Ok(ResponseEvent::ServerModel(model.clone())))
                        .await;
                    last_server_model = Some(model);
                }
                if let Some(verifications) = model_verifications
                    && tx_event
                        .send(Ok(ResponseEvent::ModelVerifications(verifications)))
                        .await
                        .is_err()
                {
                    return Err(ApiError::Stream(
                        "response event consumer dropped".to_string(),
                    ));
                }
                if let Some(metadata) = turn_moderation_metadata
                    && tx_event
                        .send(Ok(ResponseEvent::TurnModerationMetadata(metadata)))
                        .await
                        .is_err()
                {
                    return Err(ApiError::Stream(
                        "response event consumer dropped".to_string(),
                    ));
                }
                if let Some(buffering) = safety_buffering
                    && tx_event
                        .send(Ok(ResponseEvent::SafetyBuffering(buffering)))
                        .await
                        .is_err()
                {
                    return Err(ApiError::Stream(
                        "response event consumer dropped".to_string(),
                    ));
                }
                match event {
                    Ok(Some(event)) => {
                        if let ResponseEvent::Created { response_id: id } = &event {
                            response_id.clone_from(id);
                        }
                        let _ = tx_event.send(Ok(event)).await;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(error.into_api_error());
                    }
                }
                if completion.is_some() {
                    if awaiting_interrupt_ack {
                        continue;
                    }
                    if matches!(
                        interrupt.clone().now_or_never(),
                        Some(ResponseInterrupt::Cancel { .. })
                    ) {
                        break;
                    }
                    // Ordinary responses deliver completion before releasing the socket.
                    // Preserve it if cancellation wins while delivery is backpressured.
                    let permit = tx_event.reserve().await;
                    if let (Ok(permit), Some(event)) = (permit, completion.take()) {
                        permit.send(Ok(event));
                    }
                    break;
                }
            }
            Message::Binary(_) => {
                return Err(ApiError::Stream("unexpected binary websocket event".into()));
            }
            Message::Close(_) => {
                return Err(ApiError::Stream(
                    "websocket closed by server before response.completed".into(),
                ));
            }
            Message::Frame(_) => {}
            Message::Ping(_) | Message::Pong(_) => {}
        }
    }

    Ok(())
}

fn safety_buffering_for_event(
    event: &ResponsesStreamEvent,
    treatment: &mut SafetyBufferingTreatment,
) -> Option<crate::common::SafetyBuffering> {
    if let Some(headers) = event.headers.as_ref().and_then(Value::as_object)
        && let Some(updated_treatment) =
            treatment_from_headers(&json_headers_to_http_headers(headers))
    {
        *treatment = updated_treatment;
    }
    event.safety_buffering(treatment)
}

async fn send_websocket_request(
    ws_stream: &mut WsStream,
    request_text: String,
    idle_timeout: Duration,
    telemetry: Option<&Arc<dyn WebsocketTelemetry>>,
    connection_reused: bool,
) -> Result<(), ApiError> {
    let request_start = Instant::now();
    let result = tokio::time::timeout(
        idle_timeout,
        ws_stream.send(Message::Text(request_text.into())),
    )
    .await
    .map_err(|_| ApiError::Stream("idle timeout sending websocket request".into()))
    .and_then(|result| result.map_err(map_ws_stream_error));

    if let Some(t) = telemetry.as_ref() {
        t.on_ws_request(
            request_start.elapsed(),
            result.as_ref().err(),
            connection_reused,
        );
    }

    result?;

    Ok(())
}

#[cfg(test)]
#[path = "response_stream_tests.rs"]
mod tests;
