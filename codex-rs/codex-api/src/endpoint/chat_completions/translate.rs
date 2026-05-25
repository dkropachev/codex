use crate::common::ResponsesApiRequest;
use crate::common::TextFormat;
use crate::error::ApiError;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Debug)]
pub(crate) struct ChatCompletionsRequest {
    pub(crate) body: Value,
    pub(crate) tool_names: ChatToolNameMap,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ChatToolNameMap {
    entries: HashMap<String, ChatToolName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatToolName {
    pub(crate) name: String,
    pub(crate) namespace: Option<String>,
    pub(crate) kind: ChatToolKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatToolKind {
    Function,
    LocalShell,
}

impl ChatToolNameMap {
    fn insert(&mut self, chat_name: String, tool_name: ChatToolName) {
        self.entries.insert(chat_name, tool_name);
    }

    pub(crate) fn resolve(&self, chat_name: &str) -> ChatToolName {
        self.entries
            .get(chat_name)
            .cloned()
            .unwrap_or_else(|| ChatToolName {
                name: chat_name.to_string(),
                namespace: None,
                kind: if chat_name == "local_shell" {
                    ChatToolKind::LocalShell
                } else {
                    ChatToolKind::Function
                },
            })
    }
}

pub(crate) fn chat_completions_request_from_responses_request(
    request: &ResponsesApiRequest,
) -> Result<ChatCompletionsRequest, ApiError> {
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(request.model.clone()));
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert(
        "stream_options".to_string(),
        json!({ "include_usage": true }),
    );
    body.insert(
        "messages".to_string(),
        Value::Array(chat_messages_from_response_items(request)?),
    );

    let (tools, tool_names) = chat_tools_from_responses_tools(&request.tools);
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
        if request.tool_choice != "none" {
            body.insert("tool_choice".to_string(), Value::String("auto".to_string()));
        }
    }

    if let Some(format) = request.text.as_ref().and_then(|text| text.format.as_ref()) {
        body.insert("response_format".to_string(), response_format(format));
    }

    Ok(ChatCompletionsRequest {
        body: Value::Object(body),
        tool_names,
    })
}

#[cfg(test)]
fn chat_completions_body_from_responses_request(
    request: &ResponsesApiRequest,
) -> Result<Value, ApiError> {
    chat_completions_request_from_responses_request(request).map(|request| request.body)
}

fn chat_messages_from_response_items(
    request: &ResponsesApiRequest,
) -> Result<Vec<Value>, ApiError> {
    let mut messages = Vec::new();
    if !request.instructions.trim().is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.instructions,
        }));
    }

    for item in &request.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                messages.push(json!({
                    "role": role,
                    "content": chat_content_from_content_items(content),
                }));
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                messages.push(assistant_tool_call_message(call_id, name, arguments));
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input,
                ..
            } => {
                messages.push(assistant_tool_call_message(call_id, name, input));
            }
            ResponseItem::LocalShellCall {
                id,
                call_id,
                action,
                ..
            } => {
                let call_id = call_id.as_ref().or(id.as_ref()).ok_or_else(|| {
                    ApiError::Stream("local_shell call is missing call_id".to_string())
                })?;
                let arguments = serde_json::to_string(action).map_err(|err| {
                    ApiError::Stream(format!("failed to encode local_shell action: {err}"))
                })?;
                messages.push(assistant_tool_call_message(
                    call_id,
                    "local_shell",
                    &arguments,
                ));
            }
            ResponseItem::FunctionCallOutput { call_id, output }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                messages.push(tool_output_message(call_id, &output.body));
            }
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                tools,
                ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": Value::Array(tools.clone()).to_string(),
                }));
            }
            ResponseItem::Reasoning { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { call_id: None, .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::GhostSnapshot { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::Other => {}
        }
    }

    Ok(messages)
}

fn assistant_tool_call_message(call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [{
            "id": call_id,
            "type": "function",
            "function": {
                "name": name,
                "arguments": arguments,
            },
        }],
    })
}

fn tool_output_message(call_id: &str, output: &FunctionCallOutputBody) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": call_id,
        "content": output.to_text().unwrap_or_default(),
    })
}

fn chat_content_from_content_items(content: &[ContentItem]) -> Value {
    if content.iter().all(|item| {
        matches!(
            item,
            ContentItem::InputText { .. } | ContentItem::OutputText { .. }
        )
    }) {
        return Value::String(text_from_content_items(content));
    }

    Value::Array(
        content
            .iter()
            .map(|item| match item {
                ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                    json!({ "type": "text", "text": text })
                }
                ContentItem::InputImage { image_url, detail } => {
                    let mut image_url_value = Map::new();
                    image_url_value.insert("url".to_string(), Value::String(image_url.clone()));
                    if let Some(detail) = detail {
                        image_url_value
                            .insert("detail".to_string(), Value::String(image_detail(*detail)));
                    }
                    json!({
                        "type": "image_url",
                        "image_url": Value::Object(image_url_value),
                    })
                }
            })
            .collect(),
    )
}

fn text_from_content_items(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn image_detail(detail: ImageDetail) -> String {
    match detail {
        ImageDetail::Auto => "auto",
        ImageDetail::Low => "low",
        ImageDetail::High => "high",
        ImageDetail::Original => "auto",
    }
    .to_string()
}

fn response_format(format: &TextFormat) -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": format.name.clone(),
            "schema": format.schema.clone(),
            "strict": format.strict,
        },
    })
}

fn chat_tools_from_responses_tools(tools: &[Value]) -> (Vec<Value>, ChatToolNameMap) {
    let mut chat_tools = Vec::new();
    let mut tool_names = ChatToolNameMap::default();
    let mut used_names = HashSet::new();

    for tool in tools {
        let Some(tool_type) = tool.get("type").and_then(Value::as_str) else {
            continue;
        };

        match tool_type {
            "function" => {
                if let Some((chat_tool, chat_name, original_name)) =
                    chat_function_tool_from_responses_tool(
                        tool,
                        /*namespace*/ None,
                        &mut used_names,
                    )
                {
                    tool_names.insert(
                        chat_name,
                        ChatToolName {
                            name: original_name,
                            namespace: None,
                            kind: ChatToolKind::Function,
                        },
                    );
                    chat_tools.push(chat_tool);
                }
            }
            "namespace" => {
                let namespace = tool.get("name").and_then(Value::as_str).unwrap_or_default();
                let Some(namespace_tools) = tool.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for namespace_tool in namespace_tools {
                    if let Some((chat_tool, chat_name, original_name)) =
                        chat_function_tool_from_responses_tool(
                            namespace_tool,
                            Some(namespace),
                            &mut used_names,
                        )
                    {
                        tool_names.insert(
                            chat_name,
                            ChatToolName {
                                name: original_name,
                                namespace: Some(namespace.to_string()),
                                kind: ChatToolKind::Function,
                            },
                        );
                        chat_tools.push(chat_tool);
                    }
                }
            }
            "local_shell" => {
                let chat_name = unique_chat_tool_name("local_shell", &mut used_names);
                tool_names.insert(
                    chat_name.clone(),
                    ChatToolName {
                        name: "local_shell".to_string(),
                        namespace: None,
                        kind: ChatToolKind::LocalShell,
                    },
                );
                chat_tools.push(local_shell_chat_tool(&chat_name));
            }
            "custom" | "tool_search" | "image_generation" | "web_search" => {}
            _ => {}
        }
    }

    (chat_tools, tool_names)
}

fn chat_function_tool_from_responses_tool(
    tool: &Value,
    namespace: Option<&str>,
    used_names: &mut HashSet<String>,
) -> Option<(Value, String, String)> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }

    let name = tool.get("name")?.as_str()?;
    let raw_chat_name = namespace
        .map(|namespace| format!("{namespace}_{name}"))
        .unwrap_or_else(|| name.to_string());
    let chat_name = unique_chat_tool_name(&raw_chat_name, used_names);

    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(chat_name.clone()));
    if let Some(description) = tool.get("description").and_then(Value::as_str) {
        function.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    if let Some(parameters) = tool.get("parameters") {
        function.insert("parameters".to_string(), parameters.clone());
    }
    if tool.get("strict").and_then(Value::as_bool) == Some(true) {
        function.insert("strict".to_string(), Value::Bool(true));
    }

    Some((
        json!({
            "type": "function",
            "function": Value::Object(function),
        }),
        chat_name,
        name.to_string(),
    ))
}

fn unique_chat_tool_name(raw_name: &str, used_names: &mut HashSet<String>) -> String {
    let sanitized = sanitize_chat_tool_name(raw_name);
    if used_names.insert(sanitized.clone()) {
        return sanitized;
    }

    for index in 2.. {
        let suffix = format!("_{index}");
        let max_base_len = 64usize.saturating_sub(suffix.len());
        let base = truncate_ascii(&sanitized, max_base_len);
        let candidate = format!("{base}{suffix}");
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
    }

    unreachable!("unbounded tool name suffix loop should always return")
}

fn sanitize_chat_tool_name(raw_name: &str) -> String {
    let sanitized = raw_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches('_');
    let sanitized = if sanitized.is_empty() {
        "tool"
    } else {
        sanitized
    };
    truncate_ascii(sanitized, /*max_len*/ 64)
}

fn truncate_ascii(value: &str, max_len: usize) -> String {
    value.chars().take(max_len).collect()
}

fn local_shell_chat_tool(name: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": "Execute a local shell command.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Command argv to execute. On Unix, use [\"bash\", \"-lc\", \"...\"] for shell scripts."
                    },
                    "working_directory": {
                        "type": "string",
                        "description": "Working directory for the command."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::LocalShellAction;
    use codex_protocol::models::LocalShellExecAction;
    use codex_protocol::models::LocalShellStatus;
    use pretty_assertions::assert_eq;

    fn request_with(input: Vec<ResponseItem>, tools: Vec<Value>) -> ResponsesApiRequest {
        ResponsesApiRequest {
            model: "deepseek-v4-flash".to_string(),
            instructions: "Be direct.".to_string(),
            input,
            tools,
            tool_choice: "auto".to_string(),
            parallel_tool_calls: false,
            reasoning: None,
            store: false,
            stream: true,
            include: Vec::new(),
            service_tier: None,
            prompt_cache_key: None,
            text: None,
            client_metadata: None,
        }
    }

    fn function_tool(name: &str) -> Value {
        json!({
            "type": "function",
            "name": name,
            "description": "Search files.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        })
    }

    #[test]
    fn translates_basic_request_to_chat_completions() {
        let request = request_with(
            vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "hello".to_string(),
                }],
                phase: None,
            }],
            vec![function_tool("search_files")],
        );

        let body = chat_completions_body_from_responses_request(&request).unwrap();

        assert_eq!(body["model"], "deepseek-v4-flash");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Be direct.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert_eq!(body["tools"][0]["function"]["name"], "search_files");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn translates_outputs_and_local_shell_tool() {
        let request = request_with(
            vec![
                ResponseItem::LocalShellCall {
                    id: None,
                    call_id: Some("call_1".to_string()),
                    status: LocalShellStatus::Completed,
                    action: LocalShellAction::Exec(LocalShellExecAction {
                        command: vec!["bash".to_string(), "-lc".to_string(), "pwd".to_string()],
                        timeout_ms: Some(1000),
                        working_directory: Some("/tmp".to_string()),
                        env: None,
                        user: None,
                    }),
                },
                ResponseItem::FunctionCallOutput {
                    call_id: "call_1".to_string(),
                    output: FunctionCallOutputPayload::from_text("/tmp".to_string()),
                },
            ],
            vec![json!({ "type": "local_shell" })],
        );

        let body = chat_completions_body_from_responses_request(&request).unwrap();

        assert_eq!(body["tools"][0]["function"]["name"], "local_shell");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "local_shell"
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["content"], "/tmp");
    }
}
