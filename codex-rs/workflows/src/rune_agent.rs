use std::sync::Arc;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use rune::Any;
use rune::Module;
use rune::runtime::Protocol;
use rune::runtime::Ref;
use rune::runtime::Value;
use rune::runtime::Vec as RuneVec;
use rune::runtime::VmResult;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;

use crate::rune_app_server::WorkflowRuneAppServer;
use crate::rune_app_server::json_value_to_rune_result;
use crate::rune_app_server::object_without_keys;
use crate::rune_app_server::rune_value_to_json;
use crate::rune_app_server::vm_result_from_future;
use crate::rune_input::normalize_user_input;
use crate::rune_tool::WorkflowRuneDynamicTool;

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneAgentHandle {
    app_server: WorkflowRuneAppServer,
    thread_id: String,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneTurnHandle {
    app_server: WorkflowRuneAppServer,
    thread_id: String,
    turn_id: String,
    initial_receiver: Arc<AsyncMutex<Option<broadcast::Receiver<JsonValue>>>>,
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneTurnStream {
    inner: Arc<WorkflowRuneTurnStreamInner>,
}

struct WorkflowRuneTurnStreamInner {
    thread_id: String,
    turn_id: String,
    receiver: AsyncMutex<Option<broadcast::Receiver<JsonValue>>>,
    closed: AsyncMutex<bool>,
}

impl WorkflowRuneAgentHandle {
    pub(crate) async fn create(app_server: WorkflowRuneAppServer, options: Value) -> Result<Self> {
        let params = thread_params(options, None)?;
        let response = app_server.request_json("thread/start", params).await?;
        Self::from_thread_response(app_server, response)
    }

    pub(crate) async fn resume(
        app_server: WorkflowRuneAppServer,
        thread_id: String,
        options: Value,
    ) -> Result<Self> {
        let params = thread_params(options, Some(thread_id))?;
        let response = app_server.request_json("thread/resume", params).await?;
        Self::from_thread_response(app_server, response)
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::run__meta)?;
        module.function_meta(Self::run_streamed__meta)?;
        module.function_meta(Self::turn__meta)?;
        module.function_meta(Self::send_input__meta)?;
        module.function_meta(Self::spawn_agent__meta)?;
        module.function_meta(Self::create_agent__meta)?;
        module.function_meta(Self::fork__meta)?;
        module.function_meta(Self::wait__meta)?;
        module.function_meta(Self::unsubscribe__meta)?;
        module.function_meta(Self::close__meta)?;
        module.field_function(&Protocol::GET, "threadId", Self::thread_id)?;
        WorkflowRuneTurnHandle::install(module)?;
        WorkflowRuneTurnStream::install(module)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::run)]
    async fn run(this: Ref<Self>, input: Value, options: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let handle = this.start_turn(input, options).await?;
            let result = handle.collect_result().await?;
            json_value_to_rune_result(result)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::runStreamed)]
    async fn run_streamed(
        this: Ref<Self>,
        input: Value,
        options: Value,
    ) -> VmResult<WorkflowRuneTurnStream> {
        vm_result_from_future(async {
            let handle = this.start_turn(input, options).await?;
            handle.stream_json().await
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::turn)]
    async fn turn(
        this: Ref<Self>,
        input: Value,
        options: Value,
    ) -> VmResult<WorkflowRuneTurnHandle> {
        vm_result_from_future(async { this.start_turn(input, options).await }).await
    }

    #[rune::function(keep, instance, path = Self::sendInput)]
    async fn send_input(
        this: Ref<Self>,
        input: Value,
        options: Value,
    ) -> VmResult<WorkflowRuneTurnHandle> {
        vm_result_from_future(async { this.start_turn(input, options).await }).await
    }

    #[rune::function(keep, instance, path = Self::spawnAgent)]
    async fn spawn_agent(this: Ref<Self>, options: Value) -> VmResult<WorkflowRuneAgentHandle> {
        vm_result_from_future(async { Self::create(this.app_server.clone(), options).await }).await
    }

    #[rune::function(keep, instance, path = Self::createAgent)]
    async fn create_agent(this: Ref<Self>, options: Value) -> VmResult<WorkflowRuneAgentHandle> {
        vm_result_from_future(async { Self::create(this.app_server.clone(), options).await }).await
    }

    #[rune::function(keep, instance, path = Self::fork)]
    async fn fork(this: Ref<Self>, options: Value) -> VmResult<WorkflowRuneAgentHandle> {
        vm_result_from_future(async {
            let params = thread_params(options, Some(this.thread_id.clone()))?;
            let response = this.app_server.request_json("thread/fork", params).await?;
            Self::from_thread_response(this.app_server.clone(), response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::wait)]
    async fn wait(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .app_server
                .request_json("thread/read", json!({ "threadId": this.thread_id.clone() }))
                .await?;
            json_value_to_rune_result(response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::unsubscribe)]
    async fn unsubscribe(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .app_server
                .request_json(
                    "thread/unsubscribe",
                    json!({ "threadId": this.thread_id.clone() }),
                )
                .await?;
            json_value_to_rune_result(response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::close)]
    async fn close(this: Ref<Self>) -> VmResult<Value> {
        Self::unsubscribe(this).await
    }

    fn thread_id(&self) -> String {
        self.thread_id.clone()
    }

    async fn start_turn(&self, input: Value, options: Value) -> Result<WorkflowRuneTurnHandle> {
        let receiver = self.app_server.subscribe_notifications().await?;
        let mut params = turn_params(&self.thread_id, input, options)?;
        params
            .as_object_mut()
            .ok_or_else(|| anyhow!("turn params must be an object"))?
            .insert(
                "threadId".to_string(),
                JsonValue::String(self.thread_id.clone()),
            );
        let response = self.app_server.request_json("turn/start", params).await?;
        let turn_id = response
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| anyhow!("turn/start response did not include turn.id"))?
            .to_string();
        Ok(WorkflowRuneTurnHandle {
            app_server: self.app_server.clone(),
            thread_id: self.thread_id.clone(),
            turn_id,
            initial_receiver: Arc::new(AsyncMutex::new(Some(receiver))),
        })
    }

    fn from_thread_response(
        app_server: WorkflowRuneAppServer,
        response: JsonValue,
    ) -> Result<Self> {
        let thread_id = response
            .get("thread")
            .and_then(|thread| thread.get("id"))
            .and_then(JsonValue::as_str)
            .ok_or_else(|| anyhow!("thread response did not include thread.id"))?
            .to_string();
        Ok(Self {
            app_server,
            thread_id,
        })
    }
}

impl WorkflowRuneTurnHandle {
    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::stream__meta)?;
        module.function_meta(Self::run__meta)?;
        module.function_meta(Self::steer__meta)?;
        module.function_meta(Self::interrupt__meta)?;
        module.field_function(&Protocol::GET, "id", Self::id)?;
        module.field_function(&Protocol::GET, "threadId", Self::thread_id)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::stream)]
    async fn stream(this: Ref<Self>) -> VmResult<WorkflowRuneTurnStream> {
        vm_result_from_future(async { this.stream_json().await }).await
    }

    #[rune::function(keep, instance, path = Self::run)]
    async fn run(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let result = this.collect_result().await?;
            json_value_to_rune_result(result)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::steer)]
    async fn steer(this: Ref<Self>, input: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .app_server
                .request_json(
                    "turn/steer",
                    json!({
                        "threadId": this.thread_id.clone(),
                        "expectedTurnId": this.turn_id.clone(),
                        "input": normalize_user_input(input)?,
                    }),
                )
                .await?;
            json_value_to_rune_result(response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::interrupt)]
    async fn interrupt(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .app_server
                .request_json(
                    "turn/interrupt",
                    json!({ "threadId": this.thread_id.clone(), "turnId": this.turn_id.clone() }),
                )
                .await?;
            json_value_to_rune_result(response)
        })
        .await
    }

    fn id(&self) -> String {
        self.turn_id.clone()
    }

    fn thread_id(&self) -> String {
        self.thread_id.clone()
    }

    async fn stream_json(&self) -> Result<WorkflowRuneTurnStream> {
        let initial_receiver = { self.initial_receiver.lock().await.take() };
        let receiver = match initial_receiver {
            Some(receiver) => receiver,
            None => self.app_server.subscribe_notifications().await?,
        };
        Ok(WorkflowRuneTurnStream {
            inner: Arc::new(WorkflowRuneTurnStreamInner {
                thread_id: self.thread_id.clone(),
                turn_id: self.turn_id.clone(),
                receiver: AsyncMutex::new(Some(receiver)),
                closed: AsyncMutex::new(false),
            }),
        })
    }

    async fn collect_result(&self) -> Result<JsonValue> {
        let stream = self.stream_json().await?;
        stream.collect_result().await
    }
}

impl WorkflowRuneTurnStream {
    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::next__meta)?;
        module.function_meta(Self::close__meta)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::next)]
    async fn next(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let value = this.next_json().await?;
            json_value_to_rune_result(value)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::close)]
    async fn close(this: Ref<Self>) -> VmResult<()> {
        vm_result_from_future(async {
            let mut closed = this.inner.closed.lock().await;
            *closed = true;
            Ok(())
        })
        .await
    }

    async fn next_json(&self) -> Result<JsonValue> {
        if *self.inner.closed.lock().await {
            return Ok(JsonValue::Null);
        }
        loop {
            let mut receiver = {
                let mut receiver = self.inner.receiver.lock().await;
                receiver
                    .take()
                    .ok_or_else(|| anyhow!("turn stream is already waiting for a notification"))?
            };
            let notification = receiver.recv().await.context("turn stream closed")?;
            {
                let mut stored_receiver = self.inner.receiver.lock().await;
                *stored_receiver = Some(receiver);
            }
            if !notification_matches_turn(&notification, &self.inner.thread_id, &self.inner.turn_id)
            {
                continue;
            }
            if notification.get("method").and_then(JsonValue::as_str) == Some("turn/completed") {
                let mut closed = self.inner.closed.lock().await;
                *closed = true;
            }
            return Ok(notification);
        }
    }

    async fn collect_result(&self) -> Result<JsonValue> {
        let mut items = Vec::new();
        let mut turn = JsonValue::Null;
        let mut usage = JsonValue::Null;
        let mut final_response = JsonValue::Null;
        let mut status = JsonValue::String("inProgress".to_string());
        loop {
            let notification = self.next_json().await?;
            if notification.is_null() {
                break;
            }
            let method = notification
                .get("method")
                .and_then(JsonValue::as_str)
                .unwrap_or_default();
            let params = notification
                .get("params")
                .cloned()
                .unwrap_or(JsonValue::Null);
            match method {
                "item/completed" => {
                    if let Some(item) = params.get("item").cloned() {
                        if item.get("type").and_then(JsonValue::as_str) == Some("agentMessage")
                            && let Some(text) = item.get("text").and_then(JsonValue::as_str)
                            && !text.trim().is_empty()
                        {
                            final_response = JsonValue::String(text.to_string());
                        }
                        items.push(item);
                    }
                }
                "thread/tokenUsage/updated" => {
                    usage = params
                        .get("tokenUsage")
                        .or_else(|| params.get("usage"))
                        .cloned()
                        .unwrap_or(params);
                }
                "error" => {
                    let message = params
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(JsonValue::as_str)
                        .unwrap_or("turn failed");
                    anyhow::bail!("{message}");
                }
                "turn/completed" => {
                    turn = params.get("turn").cloned().unwrap_or(JsonValue::Null);
                    status = turn.get("status").cloned().unwrap_or(JsonValue::Null);
                    if status == JsonValue::String("failed".to_string()) {
                        let message = turn
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(JsonValue::as_str)
                            .unwrap_or("turn failed");
                        anyhow::bail!("{message}");
                    }
                    break;
                }
                _ => {}
            }
        }
        Ok(json!({
            "threadId": self.inner.thread_id,
            "turn": turn,
            "items": items,
            "finalResponse": final_response,
            "usage": usage,
            "status": status,
        }))
    }
}

fn thread_params(options: Value, thread_id: Option<String>) -> Result<JsonValue> {
    let mut params = object_without_keys(&options, &["tools"])?;
    let dynamic_tools = collect_dynamic_tools(&options)?;
    let object = params
        .as_object_mut()
        .ok_or_else(|| anyhow!("thread options must be an object"))?;
    if let Some(thread_id) = thread_id {
        object.insert("threadId".to_string(), JsonValue::String(thread_id));
    }
    if !dynamic_tools.is_empty() {
        object.insert("dynamicTools".to_string(), JsonValue::Array(dynamic_tools));
    }
    Ok(params)
}

fn turn_params(thread_id: &str, input: Value, options: Value) -> Result<JsonValue> {
    let mut params = object_without_keys(&options, &[])?;
    let object = params
        .as_object_mut()
        .ok_or_else(|| anyhow!("turn options must be an object"))?;
    object.insert(
        "threadId".to_string(),
        JsonValue::String(thread_id.to_string()),
    );
    object.insert(
        "input".to_string(),
        JsonValue::Array(normalize_user_input(input)?),
    );
    Ok(params)
}

fn collect_dynamic_tools(options: &Value) -> Result<Vec<JsonValue>> {
    let Ok(object) = options.borrow_ref::<rune::runtime::Object>() else {
        return Ok(Vec::new());
    };
    let Some(value) = object.get("tools") else {
        return Ok(Vec::new());
    };
    let items = value
        .borrow_ref::<RuneVec>()
        .map_err(|_| anyhow!("agent options tools must be an array"))?;
    let mut specs = Vec::new();
    for item in items.iter() {
        if let Some(tool) = WorkflowRuneDynamicTool::from_value(item) {
            specs.push(tool.spec());
            continue;
        }
        specs.push(rune_value_to_json(item.clone())?);
    }
    Ok(specs)
}

fn notification_matches_turn(notification: &JsonValue, thread_id: &str, turn_id: &str) -> bool {
    let method = notification
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let Some(params) = notification.get("params") else {
        return false;
    };
    let notification_thread_id = params.get("threadId").and_then(JsonValue::as_str);
    if notification_thread_id != Some(thread_id) {
        return false;
    }
    if method == "thread/tokenUsage/updated" {
        return true;
    }
    let notification_turn_id = params
        .get("turnId")
        .and_then(JsonValue::as_str)
        .or_else(|| {
            params
                .get("turn")
                .and_then(|turn| turn.get("id"))
                .and_then(JsonValue::as_str)
        });
    notification_turn_id == Some(turn_id)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::sync::broadcast;

    use super::WorkflowRuneAgentHandle;
    use super::WorkflowRuneTurnStream;
    use super::WorkflowRuneTurnStreamInner;
    use crate::rune_app_server::WorkflowRuneAppServer;
    use crate::rune_app_server::json_value_to_rune_result;

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn run_collects_turn_result_from_notifications() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let temp = TempDir::new().expect("temp dir");
                let server_path = temp.path().join("fake-codex");
                fs::write(
                    &server_path,
                    r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"id":"rune-initialize","result":{"userAgent":"fake/1","serverInfo":{"name":"fake","version":"1"}}}'
      ;;
    *'"method":"initialized"'*)
      ;;
    *'"method":"thread/start"'*)
      printf '%s\n' '{"id":"rune-request-1","result":{"thread":{"id":"thread-1"}}}'
      ;;
    *'"method":"turn/start"'*)
      printf '%s\n' '{"id":"rune-request-2","result":{"turn":{"id":"turn-1","items":[],"status":"inProgress"}}}'
      printf '%s\n' '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","completedAtMs":1,"item":{"type":"agentMessage","id":"item-1","text":"done"}}}'
      printf '%s\n' '{"method":"thread/tokenUsage/updated","params":{"threadId":"thread-1","tokenUsage":{"last":{"totalTokens":9}}}}'
      printf '%s\n' '{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}'
      ;;
  esac
done
"#,
                )
                .expect("fake server");
                let mut permissions = fs::metadata(&server_path).expect("metadata").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&server_path, permissions).expect("chmod");

                let app_server = WorkflowRuneAppServer::new(
                    server_path.display().to_string(),
                    temp.path().display().to_string(),
                );
                let agent = WorkflowRuneAgentHandle::create(
                    app_server,
                    json_value_to_rune_result(json!({})).expect("options"),
                )
                .await
                .expect("agent");
                let turn = agent
                    .start_turn(
                        json_value_to_rune_result(json!("hello")).expect("input"),
                        json_value_to_rune_result(json!({})).expect("options"),
                    )
                    .await
                    .expect("turn");
                let result = turn.collect_result().await.expect("result");

                assert_eq!(
                    result,
                    json!({
                        "threadId": "thread-1",
                        "turn": { "id": "turn-1", "items": [], "status": "completed" },
                        "items": [{ "type": "agentMessage", "id": "item-1", "text": "done" }],
                        "finalResponse": "done",
                        "usage": { "last": { "totalTokens": 9 } },
                        "status": "completed",
                    })
                );
            })
            .await;
    }

    #[tokio::test]
    async fn turn_stream_filters_notifications_to_matching_turn() {
        let (sender, receiver) = broadcast::channel(8);
        let stream = WorkflowRuneTurnStream {
            inner: Arc::new(WorkflowRuneTurnStreamInner {
                thread_id: "thread-1".to_string(),
                turn_id: "turn-1".to_string(),
                receiver: AsyncMutex::new(Some(receiver)),
                closed: AsyncMutex::new(false),
            }),
        };

        sender
            .send(json!({
                "method": "item/completed",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "other-turn",
                    "item": { "id": "ignored" },
                },
            }))
            .expect("send ignored notification");
        sender
            .send(json!({
                "method": "item/completed",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "kept" },
                },
            }))
            .expect("send matching notification");

        assert_eq!(
            stream.next_json().await.expect("next notification"),
            json!({
                "method": "item/completed",
                "params": {
                    "threadId": "thread-1",
                    "turnId": "turn-1",
                    "item": { "id": "kept" },
                },
            })
        );
    }
}
