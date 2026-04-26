use std::path::PathBuf;
use std::sync::Arc;

use codex_app_server_protocol::PluginEventNotification;
use codex_app_server_protocol::ServerNotification;
use codex_uds::UnixListener;
use codex_uds::prepare_private_socket_directory;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::outgoing_message::OutgoingMessageSender;

pub(crate) struct PluginRuntimeServer {
    socket_path: PathBuf,
    socket_dir: PathBuf,
    task: JoinHandle<()>,
}

impl PluginRuntimeServer {
    pub(crate) async fn start(
        plugin_id: String,
        thread_id: Option<String>,
        default_config: Value,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> std::io::Result<Self> {
        let socket_dir = std::env::temp_dir().join(format!(
            "codex-plugin-runtime-{}",
            uuid::Uuid::now_v7().as_simple()
        ));
        prepare_private_socket_directory(&socket_dir).await?;
        let socket_path = socket_dir.join("runtime.sock");
        let mut listener = UnixListener::bind(&socket_path).await?;
        let state = Arc::new(RuntimeState {
            plugin_id,
            thread_id,
            default_config,
            session_config: RwLock::new(Value::Object(Default::default())),
            outgoing,
        });
        let task = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                while let Ok(stream) = listener.accept().await {
                    tokio::spawn(handle_connection(stream, Arc::clone(&state)));
                }
            }
        });
        Ok(Self {
            socket_path,
            socket_dir,
            task,
        })
    }

    pub(crate) fn socket_path(&self) -> String {
        self.socket_path.display().to_string()
    }
}

impl Drop for PluginRuntimeServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

struct RuntimeState {
    plugin_id: String,
    thread_id: Option<String>,
    default_config: Value,
    session_config: RwLock<Value>,
    outgoing: Arc<OutgoingMessageSender>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeResponse {
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn handle_connection(stream: codex_uds::UnixStream, state: Arc<RuntimeState>) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let response = match serde_json::from_str::<RuntimeRequest>(&line) {
            Ok(request) => dispatch_request(request, Arc::clone(&state)).await,
            Err(err) => RuntimeResponse {
                id: None,
                result: None,
                error: Some(format!("invalid runtime request: {err}")),
            },
        };
        let Ok(response_json) = serde_json::to_string(&response) else {
            break;
        };
        if writer.write_all(response_json.as_bytes()).await.is_err()
            || writer.write_all(b"\n").await.is_err()
        {
            break;
        }
    }
}

async fn dispatch_request(request: RuntimeRequest, state: Arc<RuntimeState>) -> RuntimeResponse {
    let result = match request.method.as_str() {
        "plugin.config.read" => Ok(read_config(&state).await),
        "plugin.config.writeSession" => write_session_config(&state, request.params).await,
        "plugin.event.emit" => emit_event(&state, request.params).await,
        "model.structuredCall" => {
            Err("structured model calls are not available from this runtime yet".to_string())
        }
        method => Err(format!("unknown runtime method: {method}")),
    };
    match result {
        Ok(result) => RuntimeResponse {
            id: request.id,
            result: Some(result),
            error: None,
        },
        Err(error) => RuntimeResponse {
            id: request.id,
            result: None,
            error: Some(error),
        },
    }
}

async fn read_config(state: &RuntimeState) -> Value {
    let mut merged = state.default_config.clone();
    let session_config = state.session_config.read().await;
    merge_json(&mut merged, &session_config);
    merged
}

async fn write_session_config(state: &RuntimeState, params: Value) -> Result<Value, String> {
    let update = params
        .get("config")
        .cloned()
        .ok_or_else(|| "plugin.config.writeSession requires params.config".to_string())?;
    let mut session_config = state.session_config.write().await;
    merge_json(&mut session_config, &update);
    Ok(serde_json::json!({ "ok": true }))
}

async fn emit_event(state: &RuntimeState, params: Value) -> Result<Value, String> {
    let event = params
        .get("event")
        .and_then(Value::as_str)
        .ok_or_else(|| "plugin.event.emit requires params.event".to_string())?
        .to_string();
    let thread_id = params
        .get("threadId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| state.thread_id.clone());
    let payload = params.get("payload").cloned().unwrap_or(Value::Null);
    state
        .outgoing
        .send_server_notification(ServerNotification::PluginEvent(PluginEventNotification {
            thread_id,
            plugin_id: state.plugin_id.clone(),
            event,
            payload,
        }))
        .await;
    Ok(serde_json::json!({ "ok": true }))
}

fn merge_json(base: &mut Value, update: &Value) {
    match (base, update) {
        (Value::Object(base), Value::Object(update)) => {
            for (key, value) in update {
                merge_json(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, update) => *base = update.clone(),
    }
}
