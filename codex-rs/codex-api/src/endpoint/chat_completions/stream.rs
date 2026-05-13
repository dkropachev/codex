use super::local_shell::local_shell_response_item;
use super::translate::ChatToolKind;
use super::translate::ChatToolNameMap;
use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::rate_limits::parse_all_rate_limits;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

const REQUEST_ID_HEADER: &str = "x-request-id";
const OPENAI_MODEL_HEADER: &str = "openai-model";

pub(crate) fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
    tool_names: ChatToolNameMap,
) -> ResponseStream {
    let rate_limit_snapshots = parse_all_rate_limits(&stream_response.headers);
    let server_model = stream_response
        .headers
        .get(OPENAI_MODEL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-codex-turn-state")
            .and_then(|v| v.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        if let Some(model) = server_model {
            let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
        }
        for snapshot in rate_limit_snapshots {
            let _ = tx_event.send(Ok(ResponseEvent::RateLimits(snapshot))).await;
        }
        process_chat_completions_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
            tool_names,
        )
        .await;
    });

    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

async fn process_chat_completions_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tool_names: ChatToolNameMap,
) {
    let mut stream = stream.eventsource();
    let mut state = ChatStreamState::new(tool_names);

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                debug!("Chat completions SSE error: {e:#}");
                let _ = tx_event.send(Err(ApiError::Stream(e.to_string()))).await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "stream closed before chat completion finished".into(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream("idle timeout waiting for SSE".into())))
                    .await;
                return;
            }
        };

        trace!("Chat completions SSE event: {}", &sse.data);
        if sse.data.trim() == "[DONE]" {
            match state.finish() {
                Ok(events) => {
                    for event in events {
                        let is_completed = matches!(event, ResponseEvent::Completed { .. });
                        if tx_event.send(Ok(event)).await.is_err() || is_completed {
                            return;
                        }
                    }
                }
                Err(err) => {
                    let _ = tx_event.send(Err(err)).await;
                }
            }
            return;
        }

        let chunk: ChatCompletionChunk = match serde_json::from_str(&sse.data) {
            Ok(chunk) => chunk,
            Err(err) => {
                debug!(
                    "Failed to parse chat completions SSE event: {err}, data: {}",
                    &sse.data
                );
                continue;
            }
        };

        match state.update(chunk) {
            Ok(events) => {
                for event in events {
                    if tx_event.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
            Err(err) => {
                let _ = tx_event.send(Err(err)).await;
                return;
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChunk {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    choices: Vec<ChatCompletionChoice>,
    #[serde(default)]
    usage: Option<ChatCompletionUsage>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    #[serde(default)]
    delta: ChatCompletionDelta,
}

#[derive(Debug, Default, Deserialize)]
struct ChatCompletionDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct ChatToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChatToolCallFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct ChatToolCallFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    #[serde(default)]
    prompt_tokens_details: Option<ChatPromptTokensDetails>,
    #[serde(default, alias = "completion_tokens_detail")]
    completion_tokens_details: Option<ChatCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct ChatPromptTokensDetails {
    #[serde(default)]
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionTokensDetails {
    #[serde(default)]
    reasoning_tokens: i64,
}

impl From<ChatCompletionUsage> for TokenUsage {
    fn from(usage: ChatCompletionUsage) -> Self {
        Self {
            input_tokens: usage.prompt_tokens,
            cached_input_tokens: usage
                .prompt_tokens_details
                .map(|details| details.cached_tokens)
                .unwrap_or_default(),
            output_tokens: usage.completion_tokens,
            reasoning_output_tokens: usage
                .completion_tokens_details
                .map(|details| details.reasoning_tokens)
                .unwrap_or_default(),
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Debug, Default)]
struct ChatToolCallState {
    id: Option<String>,
    name: String,
    arguments: String,
}

#[derive(Debug)]
struct ChatStreamState {
    tool_names: ChatToolNameMap,
    response_id: Option<String>,
    server_model: Option<String>,
    usage: Option<TokenUsage>,
    created: bool,
    reasoning_started: bool,
    reasoning_done: bool,
    reasoning_text: String,
    message_started: bool,
    message_done: bool,
    message_text: String,
    tool_calls: BTreeMap<usize, ChatToolCallState>,
}

impl ChatStreamState {
    fn new(tool_names: ChatToolNameMap) -> Self {
        Self {
            tool_names,
            response_id: None,
            server_model: None,
            usage: None,
            created: false,
            reasoning_started: false,
            reasoning_done: false,
            reasoning_text: String::new(),
            message_started: false,
            message_done: false,
            message_text: String::new(),
            tool_calls: BTreeMap::new(),
        }
    }

    fn update(&mut self, chunk: ChatCompletionChunk) -> Result<Vec<ResponseEvent>, ApiError> {
        let mut events = Vec::new();
        if let Some(id) = chunk.id {
            self.response_id = Some(id);
        }
        if !self.created {
            self.created = true;
            events.push(ResponseEvent::Created);
        }
        if let Some(model) = chunk.model
            && self.server_model.as_deref() != Some(model.as_str())
        {
            self.server_model = Some(model.clone());
            events.push(ResponseEvent::ServerModel(model));
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage.into());
        }

        for choice in chunk.choices {
            let delta = choice.delta;
            if let Some(reasoning) = delta.reasoning_content
                && !reasoning.is_empty()
            {
                self.append_reasoning(reasoning, &mut events);
            }

            if let Some(content) = delta.content
                && !content.is_empty()
            {
                self.finish_reasoning(&mut events);
                self.append_message(content, &mut events);
            }

            if let Some(tool_calls) = delta.tool_calls
                && !tool_calls.is_empty()
            {
                self.finish_reasoning(&mut events);
                self.finish_message(&mut events);
                for tool_call in tool_calls {
                    self.append_tool_call(tool_call);
                }
            }
        }

        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<ResponseEvent>, ApiError> {
        let mut events = Vec::new();
        self.finish_reasoning(&mut events);
        self.finish_message(&mut events);
        let has_tool_calls = !self.tool_calls.is_empty();
        self.finish_tool_calls(&mut events)?;
        events.push(ResponseEvent::Completed {
            response_id: self
                .response_id
                .clone()
                .unwrap_or_else(|| "chatcmpl_unknown".to_string()),
            token_usage: self.usage.clone(),
            end_turn: Some(!has_tool_calls),
        });
        Ok(events)
    }

    fn response_id(&self) -> &str {
        self.response_id.as_deref().unwrap_or("chatcmpl_unknown")
    }

    fn reasoning_item(&self, content: Option<Vec<ReasoningItemContent>>) -> ResponseItem {
        ResponseItem::Reasoning {
            id: format!("rs_{}", self.response_id()),
            summary: Vec::new(),
            content,
            encrypted_content: None,
        }
    }

    fn append_reasoning(&mut self, delta: String, events: &mut Vec<ResponseEvent>) {
        if !self.reasoning_started || self.reasoning_done {
            self.reasoning_started = true;
            self.reasoning_done = false;
            self.reasoning_text.clear();
            events.push(ResponseEvent::OutputItemAdded(
                self.reasoning_item(Some(Vec::new())),
            ));
        }
        self.reasoning_text.push_str(&delta);
        events.push(ResponseEvent::ReasoningContentDelta {
            delta,
            content_index: 0,
        });
    }

    fn finish_reasoning(&mut self, events: &mut Vec<ResponseEvent>) {
        if !self.reasoning_started || self.reasoning_done {
            return;
        }
        self.reasoning_done = true;
        events.push(ResponseEvent::OutputItemDone(self.reasoning_item(Some(
            vec![ReasoningItemContent::ReasoningText {
                text: self.reasoning_text.clone(),
            }],
        ))));
    }

    fn append_message(&mut self, delta: String, events: &mut Vec<ResponseEvent>) {
        if !self.message_started || self.message_done {
            self.message_started = true;
            self.message_done = false;
            self.message_text.clear();
            events.push(ResponseEvent::OutputItemAdded(ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: Vec::new(),
                phase: None,
            }));
        }
        self.message_text.push_str(&delta);
        events.push(ResponseEvent::OutputTextDelta(delta));
    }

    fn finish_message(&mut self, events: &mut Vec<ResponseEvent>) {
        if !self.message_started || self.message_done {
            return;
        }
        self.message_done = true;
        events.push(ResponseEvent::OutputItemDone(ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: self.message_text.clone(),
            }],
            phase: None,
        }));
    }

    fn append_tool_call(&mut self, tool_call: ChatToolCallDelta) {
        let entry = self.tool_calls.entry(tool_call.index).or_default();
        if let Some(id) = tool_call.id {
            entry.id = Some(id);
        }
        if let Some(function) = tool_call.function {
            if let Some(name) = function.name {
                entry.name.push_str(&name);
            }
            if let Some(arguments) = function.arguments {
                entry.arguments.push_str(&arguments);
            }
        }
    }

    fn finish_tool_calls(&self, events: &mut Vec<ResponseEvent>) -> Result<(), ApiError> {
        for (index, tool_call) in &self.tool_calls {
            if tool_call.name.trim().is_empty() {
                return Err(ApiError::Stream(format!(
                    "chat completion tool call {index} is missing function name"
                )));
            }
            let call_id = tool_call
                .id
                .clone()
                .unwrap_or_else(|| format!("call_{index}"));
            let tool_name = self.tool_names.resolve(&tool_call.name);
            events.push(match tool_name.kind {
                ChatToolKind::Function => {
                    ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                        id: Some(call_id.clone()),
                        name: tool_name.name,
                        namespace: tool_name.namespace,
                        arguments: tool_call.arguments.clone(),
                        call_id,
                    })
                }
                ChatToolKind::LocalShell => ResponseEvent::OutputItemDone(
                    local_shell_response_item(call_id, &tool_call.arguments)?,
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use bytes::Bytes;
    use codex_client::TransportError;
    use codex_protocol::models::LocalShellAction;
    use codex_protocol::models::LocalShellExecAction;
    use futures::StreamExt;
    use futures::stream;
    use serde_json::Value;
    use serde_json::json;

    async fn collect_events(chunks: Vec<Value>) -> Vec<ResponseEvent> {
        let mut sse = String::new();
        for chunk in chunks {
            sse.push_str("data: ");
            sse.push_str(&chunk.to_string());
            sse.push_str("\n\n");
        }
        sse.push_str("data: [DONE]\n\n");

        let stream = stream::iter(vec![Ok::<_, TransportError>(Bytes::from(sse))]).boxed();
        let (tx, mut rx) = mpsc::channel::<Result<ResponseEvent, ApiError>>(32);
        tokio::spawn(process_chat_completions_sse(
            stream,
            tx,
            Duration::from_secs(5),
            None,
            ChatToolNameMap::default(),
        ));

        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event.unwrap());
        }
        events
    }

    #[tokio::test]
    async fn process_chat_stream_emits_message_and_usage() {
        let events = collect_events(vec![
            json!({
                "id": "chatcmpl-1",
                "model": "deepseek-v4-flash",
                "choices": [{ "delta": { "role": "assistant" } }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": { "content": "hello" } }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": { "content": " world" } }],
                "usage": {
                    "prompt_tokens": 4,
                    "completion_tokens": 2,
                    "total_tokens": 6,
                    "prompt_tokens_details": { "cached_tokens": 1 },
                    "completion_tokens_detail": { "reasoning_tokens": 0 }
                }
            }),
        ])
        .await;

        assert_matches!(events[0], ResponseEvent::Created);
        assert!(
            events.iter().any(
                |event| matches!(event, ResponseEvent::ServerModel(model) if model == "deepseek-v4-flash")
            )
        );
        assert!(events.iter().any(
            |event| matches!(event, ResponseEvent::OutputTextDelta(delta) if delta == "hello")
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::OutputItemDone(ResponseItem::Message { content, .. })
                if content == &vec![ContentItem::OutputText { text: "hello world".to_string() }]
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::Completed {
                response_id,
                token_usage: Some(TokenUsage {
                    input_tokens: 4,
                    cached_input_tokens: 1,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 6,
                }),
                end_turn: Some(true),
            } if response_id == "chatcmpl-1"
        )));
    }

    #[tokio::test]
    async fn process_chat_stream_emits_reasoning_before_message() {
        let events = collect_events(vec![
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": { "reasoning_content": "think" } }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{ "delta": { "content": "answer" } }]
            }),
        ])
        .await;

        let reasoning_done = events.iter().position(|event| matches!(
            event,
            ResponseEvent::OutputItemDone(ResponseItem::Reasoning { content: Some(content), .. })
                if content == &vec![ReasoningItemContent::ReasoningText { text: "think".to_string() }]
        ));
        let message_added = events.iter().position(|event| {
            matches!(
                event,
                ResponseEvent::OutputItemAdded(ResponseItem::Message { .. })
            )
        });

        assert!(reasoning_done.is_some());
        assert!(message_added.is_some());
        assert!(reasoning_done < message_added);
    }

    #[tokio::test]
    async fn process_chat_stream_emits_function_call() {
        let events = collect_events(vec![
            json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "type": "function",
                            "function": { "name": "search_files", "arguments": "{\"query\"" }
                        }]
                    }
                }]
            }),
            json!({
                "id": "chatcmpl-1",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": ":\"foo\"}" }
                        }]
                    }
                }]
            }),
        ])
        .await;

        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }) if name == "search_files" && arguments == "{\"query\":\"foo\"}" && call_id == "call_1"
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::Completed {
                end_turn: Some(false),
                ..
            }
        )));
    }

    #[tokio::test]
    async fn process_chat_stream_emits_local_shell_call() {
        let events = collect_events(vec![json!({
            "id": "chatcmpl-1",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "local_shell",
                            "arguments": "{\"command\":[\"bash\",\"-lc\",\"pwd\"],\"working_directory\":\"/tmp\"}"
                        }
                    }]
                }
            }]
        })])
        .await;

        assert!(events.iter().any(|event| matches!(
            event,
            ResponseEvent::OutputItemDone(ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                action: LocalShellAction::Exec(LocalShellExecAction { command, working_directory, .. }),
                ..
            }) if call_id == "call_1"
                && command == &vec!["bash".to_string(), "-lc".to_string(), "pwd".to_string()]
                && working_directory.as_deref() == Some("/tmp")
        )));
    }
}
