use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use codex_utils_rustls_provider::ensure_rustls_crypto_provider;
use futures::SinkExt;
use futures::StreamExt;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use rune::Any;
use rune::Module;
use rune::runtime::Function;
use rune::runtime::Future as RuneFuture;
use rune::runtime::Object;
use rune::runtime::Protocol;
use rune::runtime::Ref;
use rune::runtime::TypeHash as _;
use rune::runtime::Value;
use rune::runtime::VmResult;
use serde_json::Map;
use serde_json::Value as JsonValue;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::Lines;
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::process::Command;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use url::Url;

const CODEX_WORKFLOW_APP_SERVER_URL_ENV: &str = "CODEX_WORKFLOW_APP_SERVER_URL";
const CODEX_APP_SERVER_URL_ENV: &str = "CODEX_APP_SERVER_URL";
const CODEX_WORKFLOW_APPROVALS_ENV: &str = "CODEX_WORKFLOW_APPROVALS";
const CLIENT_CHANNEL_CAPACITY: usize = 128;
const NOTIFICATION_CHANNEL_CAPACITY: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const RETRY_MAX_DELAY: Duration = Duration::from_secs(4);
const WS_MAX_MESSAGE_SIZE: usize = 128 << 20;

/// App-server RPC client exposed to embedded Rune workflows.
///
/// A workflow context owns one client connection. The client keeps raw
/// `request` access available while higher-level Rune SDK objects share the
/// same request, notification, dynamic-tool, and approval routing machinery.
#[derive(Clone, Any)]
#[rune(item = ::codex)]
pub(crate) struct WorkflowRuneAppServer {
    inner: Rc<WorkflowRuneAppServerInner>,
}

pub(crate) struct RegisteredDynamicTool {
    pub(crate) namespace: Option<String>,
    pub(crate) name: String,
    pub(crate) handler: Function,
}

#[derive(Clone)]
enum ConnectionSetting {
    Auto,
    Spawn,
    RequireExisting,
    ExistingUrl { app_server_url: String },
}

#[derive(Clone)]
enum ResolvedConnection {
    Spawn,
    ExistingUrl { app_server_url: String },
}

enum ApprovalMode {
    Decline,
    Delegate,
    Handler(Function),
}

struct WorkflowRuneAppServerInner {
    codex_exe: String,
    cwd: String,
    config: StdMutex<ConnectionSetting>,
    approvals: StdMutex<ApprovalMode>,
    dynamic_tools: StdMutex<HashMap<ToolKey, RegisteredDynamicTool>>,
    command_tx: AsyncMutex<Option<mpsc::Sender<ClientCommand>>>,
    notification_tx: broadcast::Sender<JsonValue>,
    raw_notification_tx: mpsc::UnboundedSender<JsonValue>,
    raw_notification_rx: AsyncMutex<Option<mpsc::UnboundedReceiver<JsonValue>>>,
    next_id: AtomicU64,
    started: AtomicBool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ToolKey {
    namespace: Option<String>,
    name: String,
}

enum ClientCommand {
    Request {
        id: String,
        method: String,
        params: JsonValue,
        response_tx: oneshot::Sender<Result<JsonValue, AppServerRpcError>>,
    },
    Notify {
        method: String,
        params: JsonValue,
        response_tx: oneshot::Sender<Result<()>>,
    },
    Respond {
        id: JsonValue,
        result: JsonValue,
    },
    Reject {
        id: JsonValue,
        error: AppServerRpcError,
    },
    Shutdown {
        response_tx: oneshot::Sender<Result<()>>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct AppServerRpcError {
    code: i64,
    message: String,
    data: Option<JsonValue>,
}

enum ReaderEvent {
    Message(JsonValue),
    Disconnected(String),
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;

struct AppServerConnection {
    writer: JsonWriter,
    reader: JsonReader,
    child: Option<Child>,
}

enum JsonWriter {
    Stdio(ChildStdin),
    WebSocket(WsWriter),
}

enum JsonReader {
    Stdio(Lines<BufReader<ChildStdout>>),
    WebSocket(WsReader),
}

impl WorkflowRuneAppServer {
    pub(crate) fn new(codex_exe: String, cwd: String) -> Self {
        let (notification_tx, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
        let (raw_notification_tx, raw_notification_rx) = mpsc::unbounded_channel();
        Self {
            inner: Rc::new(WorkflowRuneAppServerInner {
                codex_exe,
                cwd,
                config: StdMutex::new(ConnectionSetting::Auto),
                approvals: StdMutex::new(default_approval_mode()),
                dynamic_tools: StdMutex::new(HashMap::new()),
                command_tx: AsyncMutex::new(None),
                notification_tx,
                raw_notification_tx,
                raw_notification_rx: AsyncMutex::new(Some(raw_notification_rx)),
                next_id: AtomicU64::new(1),
                started: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn install(module: &mut Module) -> Result<(), rune::ContextError> {
        module.ty::<Self>()?;
        module.function_meta(Self::configure__meta)?;
        module.function_meta(Self::request__meta)?;
        module.function_meta(Self::try_request__meta)?;
        module.function_meta(Self::notify__meta)?;
        module.function_meta(Self::next_notification__meta)?;
        module.function_meta(Self::set_approvals__meta)?;
        module.function_meta(Self::retry_on_overload__meta)?;
        module.function_meta(Self::close__meta)?;
        module.field_function(&Protocol::GET, "connected", Self::connected)?;
        Ok(())
    }

    #[rune::function(keep, instance, path = Self::configure)]
    async fn configure(this: Ref<Self>, options: Value) -> VmResult<()> {
        vm_result_from_future(async {
            this.configure_json(rune_value_to_json(options)?)
                .context("failed to configure Rune app-server client")
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::request)]
    async fn request(this: Ref<Self>, method: Ref<str>, params: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .request_json(&method, rune_value_to_json(params)?)
                .await
                .with_context(|| format!("app-server request {} failed", &*method))?;
            json_value_to_rune_result(response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::tryRequest)]
    async fn try_request(this: Ref<Self>, method: Ref<str>, params: Value) -> VmResult<Value> {
        vm_result_from_future(async {
            let response = this
                .try_request_json(&method, rune_value_to_json(params)?)
                .await
                .with_context(|| format!("app-server tryRequest {} failed", &*method))?;
            json_value_to_rune_result(response)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::notify)]
    async fn notify(this: Ref<Self>, method: Ref<str>, params: Value) -> VmResult<()> {
        vm_result_from_future(async {
            this.notify_json(&method, rune_value_to_json(params)?)
                .await
                .with_context(|| format!("app-server notification {} failed", &*method))
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::nextNotification)]
    async fn next_notification(this: Ref<Self>) -> VmResult<Value> {
        vm_result_from_future(async {
            let notification = this
                .next_notification_json()
                .await
                .context("failed to read app-server notification")?;
            json_value_to_rune_result(notification)
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::setApprovals)]
    fn set_approvals(this: Ref<Self>, mode: Value) -> VmResult<()> {
        vm_result_from_result(this.set_approvals_value(mode))
    }

    #[rune::function(keep, instance, path = Self::retryOnOverload)]
    async fn retry_on_overload(
        _this: Ref<Self>,
        handler: Function,
        options: Value,
    ) -> VmResult<Value> {
        vm_result_from_future(async {
            let options = retry_options(rune_value_to_json(options)?);
            let mut attempt = 0usize;
            let mut delay = RETRY_BASE_DELAY;
            loop {
                let result = match handler
                    .call::<Value>(())
                    .into_result()
                    .map_err(anyhow::Error::from)
                {
                    Ok(value) => await_rune_value(value).await,
                    Err(err) => Err(err),
                };
                match result {
                    Ok(value) => return Ok(value),
                    Err(err) if attempt < options.max_retries && is_overload_error(&err) => {
                        attempt += 1;
                        sleep(delay).await;
                        delay = (delay * 2).min(options.max_delay);
                    }
                    Err(err) => return Err(err),
                }
            }
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::close)]
    async fn close(this: Ref<Self>) -> VmResult<()> {
        vm_result_from_future(async { this.close_json().await }).await
    }

    fn connected(&self) -> bool {
        self.inner.started.load(Ordering::SeqCst)
    }

    pub(crate) async fn request_json(&self, method: &str, params: JsonValue) -> Result<JsonValue> {
        let tx = self.command_tx().await?;
        let id = self.next_request_id();
        let (response_tx, response_rx) = oneshot::channel();
        tx.send(ClientCommand::Request {
            id,
            method: method.to_string(),
            params,
            response_tx,
        })
        .await
        .map_err(|_| anyhow!("app-server client worker is closed"))?;
        match response_rx
            .await
            .map_err(|_| anyhow!("app-server response channel is closed"))?
        {
            Ok(result) => Ok(result),
            Err(err) => Err(err.into_anyhow(method)),
        }
    }

    pub(crate) async fn try_request_json(
        &self,
        method: &str,
        params: JsonValue,
    ) -> Result<JsonValue> {
        let tx = self.command_tx().await?;
        let id = self.next_request_id();
        let (response_tx, response_rx) = oneshot::channel();
        tx.send(ClientCommand::Request {
            id,
            method: method.to_string(),
            params,
            response_tx,
        })
        .await
        .map_err(|_| anyhow!("app-server client worker is closed"))?;
        let result = response_rx
            .await
            .map_err(|_| anyhow!("app-server response channel is closed"))?;
        Ok(match result {
            Ok(result) => json!({ "ok": true, "result": result }),
            Err(error) => json!({ "ok": false, "error": error.to_json() }),
        })
    }

    pub(crate) async fn notify_json(&self, method: &str, params: JsonValue) -> Result<()> {
        let tx = self.command_tx().await?;
        let (response_tx, response_rx) = oneshot::channel();
        tx.send(ClientCommand::Notify {
            method: method.to_string(),
            params,
            response_tx,
        })
        .await
        .map_err(|_| anyhow!("app-server client worker is closed"))?;
        response_rx
            .await
            .map_err(|_| anyhow!("app-server notify channel is closed"))?
    }

    pub(crate) async fn next_notification_json(&self) -> Result<JsonValue> {
        self.command_tx().await?;
        let mut rx = {
            let mut stored_rx = self.inner.raw_notification_rx.lock().await;
            stored_rx
                .take()
                .ok_or_else(|| anyhow!("app-server notification reader is already waiting"))?
        };
        let notification = rx
            .recv()
            .await
            .ok_or_else(|| anyhow!("app-server notification channel is closed"))?;
        {
            let mut stored_rx = self.inner.raw_notification_rx.lock().await;
            *stored_rx = Some(rx);
        }
        Ok(notification)
    }

    pub(crate) async fn subscribe_notifications(&self) -> Result<broadcast::Receiver<JsonValue>> {
        self.command_tx().await?;
        Ok(self.inner.notification_tx.subscribe())
    }

    pub(crate) async fn close_json(&self) -> Result<()> {
        let tx = {
            let mut guard = self.inner.command_tx.lock().await;
            guard.take()
        };
        let Some(tx) = tx else {
            return Ok(());
        };
        let (response_tx, response_rx) = oneshot::channel();
        tx.send(ClientCommand::Shutdown { response_tx })
            .await
            .map_err(|_| anyhow!("app-server client worker is closed"))?;
        response_rx
            .await
            .map_err(|_| anyhow!("app-server shutdown channel is closed"))??;
        self.inner.started.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn register_dynamic_tool(&self, tool: RegisteredDynamicTool) -> Result<()> {
        let key = ToolKey {
            namespace: tool.namespace.clone(),
            name: tool.name.clone(),
        };
        let mut tools = self
            .inner
            .dynamic_tools
            .lock()
            .map_err(|_| anyhow!("dynamic tool registry lock was poisoned"))?;
        tools.insert(key, tool);
        Ok(())
    }

    fn configure_json(&self, options: JsonValue) -> Result<()> {
        if self.inner.started.load(Ordering::SeqCst) {
            anyhow::bail!(
                "ctx.appServer.configure(...) must be called before the first app-server use"
            );
        }
        let connection = match options.get("connection") {
            Some(JsonValue::String(value)) => parse_connection_string(value)?,
            Some(JsonValue::Object(object)) => {
                let app_server_url = object
                    .get("appServerUrl")
                    .or_else(|| object.get("app_server_url"))
                    .and_then(JsonValue::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("connection object requires appServerUrl"))?;
                ConnectionSetting::ExistingUrl {
                    app_server_url: app_server_url.to_string(),
                }
            }
            Some(_) => anyhow::bail!("connection must be a string or object"),
            None => ConnectionSetting::Auto,
        };
        let mut guard = self
            .inner
            .config
            .lock()
            .map_err(|_| anyhow!("app-server config lock was poisoned"))?;
        *guard = connection;
        Ok(())
    }

    fn set_approvals_value(&self, mode: Value) -> Result<()> {
        let approval_mode = approval_mode_from_value(&mode)?;
        if matches!(approval_mode, ApprovalMode::Delegate)
            && matches!(self.resolve_connection()?, ResolvedConnection::Spawn)
        {
            anyhow::bail!(
                "ctx.appServer.setApprovals(\"delegate\") requires an existing app-server connection"
            );
        }
        let mut approvals = self
            .inner
            .approvals
            .lock()
            .map_err(|_| anyhow!("app-server approvals lock was poisoned"))?;
        *approvals = approval_mode;
        Ok(())
    }

    async fn command_tx(&self) -> Result<mpsc::Sender<ClientCommand>> {
        if let Some(tx) = self.inner.command_tx.lock().await.as_ref().cloned() {
            return Ok(tx);
        }
        self.inner.started.store(true, Ordering::SeqCst);
        let resolved = self.resolve_connection()?;
        let connection =
            AppServerConnection::connect(resolved, &self.inner.codex_exe, &self.inner.cwd).await?;
        let mut guard = self.inner.command_tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            return Ok(tx.clone());
        }
        let tx = start_client_worker(connection, Rc::clone(&self.inner));
        *guard = Some(tx.clone());
        Ok(tx)
    }

    fn resolve_connection(&self) -> Result<ResolvedConnection> {
        let setting = self
            .inner
            .config
            .lock()
            .map_err(|_| anyhow!("app-server config lock was poisoned"))?
            .clone();
        match setting {
            ConnectionSetting::Auto => match configured_app_server_url() {
                Some(app_server_url) => Ok(ResolvedConnection::ExistingUrl { app_server_url }),
                None => Ok(ResolvedConnection::Spawn),
            },
            ConnectionSetting::Spawn => Ok(ResolvedConnection::Spawn),
            ConnectionSetting::RequireExisting => configured_app_server_url()
                .map(|app_server_url| ResolvedConnection::ExistingUrl { app_server_url })
                .ok_or_else(|| {
                    anyhow!(
                        "app-server connection requires {CODEX_WORKFLOW_APP_SERVER_URL_ENV} or {CODEX_APP_SERVER_URL_ENV}"
                    )
                }),
            ConnectionSetting::ExistingUrl { app_server_url } => {
                Ok(ResolvedConnection::ExistingUrl { app_server_url })
            }
        }
    }

    fn next_request_id(&self) -> String {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        format!("rune-request-{id}")
    }
}

impl WorkflowRuneAppServerInner {
    async fn handle_server_request(&self, request: JsonValue) -> ServerRequestReply {
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        let params = request.get("params").cloned().unwrap_or(JsonValue::Null);
        match method.as_str() {
            "item/tool/call" => {
                self.handle_dynamic_tool_call(id, method, params, request)
                    .await
            }
            "account/chatgptAuthTokens/refresh" => ServerRequestReply::Reject {
                id,
                error: AppServerRpcError::new(
                    -32000,
                    "chatgpt auth token refresh is not supported for Rune workflows",
                ),
            },
            "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/requestUserInput"
            | "mcpServer/elicitation/request" => {
                self.handle_approval_request(id, method, params, request)
                    .await
            }
            _ => ServerRequestReply::Reject {
                id,
                error: AppServerRpcError::new(
                    -32601,
                    format!("No Rune handler for app-server request {method}"),
                ),
            },
        }
    }

    async fn handle_dynamic_tool_call(
        &self,
        id: JsonValue,
        method: String,
        params: JsonValue,
        request: JsonValue,
    ) -> ServerRequestReply {
        let namespace = params
            .get("namespace")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string);
        let tool_name = params
            .get("tool")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        let key = ToolKey {
            namespace: namespace.clone(),
            name: tool_name.clone(),
        };
        let arguments = params.get("arguments").cloned().unwrap_or(JsonValue::Null);
        let call = json!({
            "type": "dynamicToolCall",
            "method": method,
            "id": id,
            "callId": params.get("callId").cloned().unwrap_or(JsonValue::Null),
            "threadId": params.get("threadId").cloned().unwrap_or(JsonValue::Null),
            "turnId": params.get("turnId").cloned().unwrap_or(JsonValue::Null),
            "namespace": namespace,
            "tool": tool_name,
            "rawRequest": request,
        });
        let arguments = match json_value_to_rune_result(arguments) {
            Ok(arguments) => arguments,
            Err(err) => {
                return ServerRequestReply::Respond {
                    id,
                    result: failed_tool_result(format!("{err:#}")),
                };
            }
        };
        let call = match json_value_to_rune_result(call) {
            Ok(call) => call,
            Err(err) => {
                return ServerRequestReply::Respond {
                    id,
                    result: failed_tool_result(format!("{err:#}")),
                };
            }
        };
        let result = {
            let tools = match self
                .dynamic_tools
                .lock()
                .map_err(|_| anyhow!("dynamic tool registry lock was poisoned"))
            {
                Ok(tools) => tools,
                Err(err) => {
                    return ServerRequestReply::Respond {
                        id,
                        result: failed_tool_result(err.to_string()),
                    };
                }
            };
            let Some(tool) = tools.get(&key) else {
                return ServerRequestReply::Respond {
                    id,
                    result: failed_tool_result(format!(
                        "missing Rune dynamic tool handler for `{tool_name}`"
                    )),
                };
            };
            tool.handler
                .call::<Value>((arguments, call))
                .into_result()
                .map_err(anyhow::Error::from)
        };
        let result = match result {
            Ok(value) => await_rune_value(value)
                .await
                .and_then(normalize_dynamic_tool_result),
            Err(err) => Err(err),
        };
        match result {
            Ok(result) => ServerRequestReply::Respond { id, result },
            Err(err) => ServerRequestReply::Respond {
                id,
                result: failed_tool_result(format!("{err:#}")),
            },
        }
    }

    async fn handle_approval_request(
        &self,
        id: JsonValue,
        method: String,
        params: JsonValue,
        request: JsonValue,
    ) -> ServerRequestReply {
        let approvals = self.approvals.lock();
        let Ok(mode) = approvals.as_ref() else {
            return ServerRequestReply::Respond {
                id,
                result: decline_response_for_method(&method),
            };
        };
        match &**mode {
            ApprovalMode::Decline => ServerRequestReply::Respond {
                id,
                result: decline_response_for_method(&method),
            },
            ApprovalMode::Delegate => ServerRequestReply::Reject {
                id,
                error: AppServerRpcError::new(
                    -32000,
                    "approval delegation is not available on this Rune app-server connection",
                ),
            },
            ApprovalMode::Handler(handler) => {
                let payload = json!({
                    "type": approval_type_for_method(&method),
                    "method": method,
                    "id": id,
                    "params": params,
                    "rawRequest": request,
                });
                let payload = match json_value_to_rune_result(payload) {
                    Ok(payload) => payload,
                    Err(err) => {
                        return ServerRequestReply::Reject {
                            id,
                            error: AppServerRpcError::new(-32000, format!("{err:#}")),
                        };
                    }
                };
                let result = handler
                    .call::<Value>((payload,))
                    .into_result()
                    .map_err(anyhow::Error::from);
                drop(approvals);
                match match result {
                    Ok(value) => await_rune_value(value).await.and_then(rune_value_to_json),
                    Err(err) => Err(err),
                } {
                    Ok(result) => ServerRequestReply::Respond { id, result },
                    Err(err) => ServerRequestReply::Reject {
                        id,
                        error: AppServerRpcError::new(-32000, format!("{err:#}")),
                    },
                }
            }
        }
    }
}

enum ServerRequestReply {
    Respond {
        id: JsonValue,
        result: JsonValue,
    },
    Reject {
        id: JsonValue,
        error: AppServerRpcError,
    },
}

impl AppServerRpcError {
    fn new(message_code: i64, message: impl Into<String>) -> Self {
        Self {
            code: message_code,
            message: message.into(),
            data: None,
        }
    }

    fn from_json(value: &JsonValue) -> Self {
        Self {
            code: value
                .get("code")
                .and_then(JsonValue::as_i64)
                .unwrap_or(-32000),
            message: value
                .get("message")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown app-server error")
                .to_string(),
            data: value.get("data").cloned(),
        }
    }

    fn into_anyhow(self, method: &str) -> anyhow::Error {
        anyhow!("{method} failed: {}", self.message)
    }

    fn to_json(&self) -> JsonValue {
        let mut error = json!({
            "code": self.code,
            "message": self.message,
        });
        if let Some(data) = &self.data {
            error["data"] = data.clone();
        }
        error
    }
}

impl AppServerConnection {
    async fn connect(resolved: ResolvedConnection, codex_exe: &str, cwd: &str) -> Result<Self> {
        match resolved {
            ResolvedConnection::Spawn => Self::spawn_stdio(codex_exe, cwd).await,
            ResolvedConnection::ExistingUrl { app_server_url } => {
                Self::connect_websocket(&app_server_url).await
            }
        }
    }

    async fn spawn_stdio(codex_exe: &str, cwd: &str) -> Result<Self> {
        let mut command = Command::new(codex_exe);
        command
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(PathBuf::from(cwd))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start app-server with {codex_exe}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("app-server stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("app-server stdout was not piped"))?;
        let mut connection = Self {
            writer: JsonWriter::Stdio(stdin),
            reader: JsonReader::Stdio(BufReader::new(stdout).lines()),
            child: Some(child),
        };
        connection.initialize().await?;
        Ok(connection)
    }

    async fn connect_websocket(app_server_url: &str) -> Result<Self> {
        let url = Url::parse(app_server_url)
            .with_context(|| format!("invalid app-server websocket URL `{app_server_url}`"))?;
        if !matches!(url.scheme(), "ws" | "wss") {
            anyhow::bail!("app-server URLs must use ws:// or wss://, got `{app_server_url}`");
        }
        let mut request = url
            .as_str()
            .into_client_request()
            .with_context(|| format!("invalid app-server websocket URL `{app_server_url}`"))?;
        if let Some(token) = url
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then(|| value.to_string()))
        {
            request.headers_mut().insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("invalid app-server websocket token")?,
            );
        }
        ensure_rustls_crypto_provider();
        let config = WebSocketConfig::default()
            .max_frame_size(Some(WS_MAX_MESSAGE_SIZE))
            .max_message_size(Some(WS_MAX_MESSAGE_SIZE));
        let (stream, _) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            connect_async_with_config(request, Some(config), /*disable_nagle*/ false),
        )
        .await
        .with_context(|| format!("timed out connecting to app-server at `{app_server_url}`"))?
        .with_context(|| format!("failed to connect to app-server at `{app_server_url}`"))?;
        let (writer, reader) = stream.split();
        let mut connection = Self {
            writer: JsonWriter::WebSocket(writer),
            reader: JsonReader::WebSocket(reader),
            child: None,
        };
        connection.initialize().await?;
        Ok(connection)
    }

    async fn initialize(&mut self) -> Result<()> {
        self.writer
            .write_json(&json!({
                "id": "rune-initialize",
                "method": "initialize",
                "params": {
                    "clientInfo": {
                        "name": "codex_rune_workflow",
                        "title": "Codex Rune Workflow",
                        "version": "0.0.0-dev"
                    },
                    "capabilities": {
                        "experimentalApi": true
                    }
                }
            }))
            .await?;
        loop {
            let message = self.reader.read_json().await?;
            let id_matches =
                message.get("id").and_then(JsonValue::as_str) == Some("rune-initialize");
            if !id_matches {
                continue;
            }
            if let Some(error) = message.get("error") {
                anyhow::bail!(
                    "initialize failed: {}",
                    error
                        .get("message")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("unknown app-server error")
                );
            }
            break;
        }
        self.writer
            .write_json(&json!({ "method": "initialized" }))
            .await
    }
}

impl JsonWriter {
    async fn write_json(&mut self, value: &JsonValue) -> Result<()> {
        let line = serde_json::to_string(value)?;
        match self {
            Self::Stdio(stdin) => {
                stdin
                    .write_all(line.as_bytes())
                    .await
                    .context("failed to write app-server message")?;
                stdin
                    .write_all(b"\n")
                    .await
                    .context("failed to terminate app-server message")?;
                stdin
                    .flush()
                    .await
                    .context("failed to flush app-server message")
            }
            Self::WebSocket(writer) => writer
                .send(Message::Text(line.into()))
                .await
                .context("failed to write app-server websocket message"),
        }
    }

    async fn close(&mut self) -> Result<()> {
        match self {
            Self::Stdio(stdin) => stdin
                .shutdown()
                .await
                .context("failed to close app-server stdin"),
            Self::WebSocket(writer) => writer
                .send(Message::Close(None))
                .await
                .context("failed to close app-server websocket"),
        }
    }
}

impl JsonReader {
    async fn read_json(&mut self) -> Result<JsonValue> {
        loop {
            let payload = match self {
                Self::Stdio(stdout) => {
                    let Some(line) = stdout
                        .next_line()
                        .await
                        .context("failed to read app-server stdout")?
                    else {
                        anyhow::bail!("app-server closed stdout");
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    line
                }
                Self::WebSocket(reader) => match reader.next().await {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(_)))
                    | Some(Ok(Message::Ping(_)))
                    | Some(Ok(Message::Pong(_)))
                    | Some(Ok(Message::Frame(_))) => continue,
                    Some(Ok(Message::Close(frame))) => {
                        let reason = frame
                            .as_ref()
                            .map(|frame| frame.reason.to_string())
                            .filter(|reason| !reason.is_empty())
                            .unwrap_or_else(|| "connection closed".to_string());
                        anyhow::bail!("app-server websocket closed: {reason}");
                    }
                    Some(Err(err)) => anyhow::bail!("app-server websocket failed: {err}"),
                    None => anyhow::bail!("app-server websocket closed"),
                },
            };
            return serde_json::from_str::<JsonValue>(&payload)
                .with_context(|| format!("failed to decode app-server JSON message `{payload}`"));
        }
    }
}

fn start_client_worker(
    connection: AppServerConnection,
    inner: Rc<WorkflowRuneAppServerInner>,
) -> mpsc::Sender<ClientCommand> {
    let AppServerConnection {
        mut writer,
        reader,
        mut child,
    } = connection;
    let (command_tx, mut command_rx) = mpsc::channel::<ClientCommand>(CLIENT_CHANNEL_CAPACITY);
    let command_tx_for_worker = command_tx.clone();
    let (reader_tx, mut reader_rx) = mpsc::unbounded_channel::<ReaderEvent>();
    tokio::task::spawn_local(async move {
        run_reader(reader, reader_tx).await;
    });
    tokio::task::spawn_local(async move {
        let mut pending =
            HashMap::<String, oneshot::Sender<Result<JsonValue, AppServerRpcError>>>::new();
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some(command) = command else {
                        let _ = writer.close().await;
                        break;
                    };
                    match command {
                        ClientCommand::Request { id, method, params, response_tx } => {
                            pending.insert(id.clone(), response_tx);
                            if let Err(err) = writer.write_json(&json!({
                                "id": id,
                                "method": method,
                                "params": params,
                            })).await {
                                fail_pending(&mut pending, err.to_string());
                                break;
                            }
                        }
                        ClientCommand::Notify { method, params, response_tx } => {
                            let result = writer.write_json(&json!({
                                "method": method,
                                "params": params,
                            })).await;
                            let _ = response_tx.send(result);
                        }
                        ClientCommand::Respond { id, result } => {
                            let _ = writer.write_json(&json!({ "id": id, "result": result })).await;
                        }
                        ClientCommand::Reject { id, error } => {
                            let _ = writer.write_json(&json!({ "id": id, "error": error.to_json() })).await;
                        }
                        ClientCommand::Shutdown { response_tx } => {
                            let result = writer.close().await;
                            if let Some(child) = child.as_mut() {
                                let _ = child.start_kill();
                                let _ = child.wait().await;
                            }
                            let _ = response_tx.send(result);
                            break;
                        }
                    }
                }
                event = reader_rx.recv() => {
                    match event {
                        Some(ReaderEvent::Message(message)) => {
                            handle_incoming_message(
                                message,
                                &inner,
                                &command_tx_for_worker,
                                &mut pending,
                            )
                            .await;
                        }
                        Some(ReaderEvent::Disconnected(message)) => {
                            fail_pending(&mut pending, message);
                            break;
                        }
                        None => {
                            fail_pending(&mut pending, "app-server reader stopped".to_string());
                            break;
                        }
                    }
                }
            }
        }
    });
    command_tx
}

async fn run_reader(mut reader: JsonReader, tx: mpsc::UnboundedSender<ReaderEvent>) {
    loop {
        match reader.read_json().await {
            Ok(message) => {
                if tx.send(ReaderEvent::Message(message)).is_err() {
                    break;
                }
            }
            Err(err) => {
                let _ = tx.send(ReaderEvent::Disconnected(format!("{err:#}")));
                break;
            }
        }
    }
}

async fn handle_incoming_message(
    message: JsonValue,
    inner: &Rc<WorkflowRuneAppServerInner>,
    command_tx: &mpsc::Sender<ClientCommand>,
    pending: &mut HashMap<String, oneshot::Sender<Result<JsonValue, AppServerRpcError>>>,
) {
    if message.get("id").is_some() && message.get("method").is_some() {
        let reply = inner.handle_server_request(message).await;
        match reply {
            ServerRequestReply::Respond { id, result } => {
                let _ = command_tx.send(ClientCommand::Respond { id, result }).await;
            }
            ServerRequestReply::Reject { id, error } => {
                let _ = command_tx.send(ClientCommand::Reject { id, error }).await;
            }
        }
        return;
    }
    if message.get("method").is_some() {
        let _ = inner.raw_notification_tx.send(message.clone());
        let _ = inner.notification_tx.send(message);
        return;
    }
    let Some(id) = message
        .get("id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
    else {
        return;
    };
    let Some(response_tx) = pending.remove(&id) else {
        return;
    };
    if let Some(error) = message.get("error") {
        let _ = response_tx.send(Err(AppServerRpcError::from_json(error)));
        return;
    }
    let _ = response_tx.send(Ok(message
        .get("result")
        .cloned()
        .unwrap_or(JsonValue::Null)));
}

fn fail_pending(
    pending: &mut HashMap<String, oneshot::Sender<Result<JsonValue, AppServerRpcError>>>,
    message: String,
) {
    for (_, response_tx) in pending.drain() {
        let _ = response_tx.send(Err(AppServerRpcError::new(-32000, message.clone())));
    }
}

fn parse_connection_string(value: &str) -> Result<ConnectionSetting> {
    match value {
        "auto" => Ok(ConnectionSetting::Auto),
        "spawn" => Ok(ConnectionSetting::Spawn),
        "require-existing" => Ok(ConnectionSetting::RequireExisting),
        _ => anyhow::bail!(
            "connection must be \"auto\", \"spawn\", \"require-existing\", or {{ appServerUrl }}"
        ),
    }
}

fn configured_app_server_url() -> Option<String> {
    env::var(CODEX_WORKFLOW_APP_SERVER_URL_ENV)
        .ok()
        .or_else(|| env::var(CODEX_APP_SERVER_URL_ENV).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_approval_mode() -> ApprovalMode {
    match env::var(CODEX_WORKFLOW_APPROVALS_ENV)
        .unwrap_or_else(|_| "decline".to_string())
        .as_str()
    {
        "delegate" => ApprovalMode::Delegate,
        _ => ApprovalMode::Decline,
    }
}

fn approval_mode_from_value(value: &Value) -> Result<ApprovalMode> {
    if let Ok(mode) = rune::from_value::<String>(value.clone()) {
        return match mode.as_str() {
            "decline" => Ok(ApprovalMode::Decline),
            "delegate" => Ok(ApprovalMode::Delegate),
            _ => anyhow::bail!(
                "approval mode must be \"decline\", \"delegate\", or a handler object"
            ),
        };
    }
    let object = value
        .borrow_ref::<Object>()
        .map_err(|_| anyhow!("approval mode must be a string or object"))?;
    let mode = object
        .get("mode")
        .and_then(|value| rune::from_value::<String>(value.clone()).ok())
        .ok_or_else(|| anyhow!("approval handler object requires mode"))?;
    if mode != "handler" {
        anyhow::bail!("approval handler object mode must be \"handler\"");
    }
    let handler = object
        .get("onApproval")
        .and_then(|value| rune::from_value::<Function>(value.clone()).ok())
        .ok_or_else(|| anyhow!("approval handler object requires onApproval"))?;
    Ok(ApprovalMode::Handler(handler))
}

fn approval_type_for_method(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" => "command",
        "item/fileChange/requestApproval" => "fileChange",
        "item/permissions/requestApproval" => "permissions",
        "item/tool/requestUserInput" => "userInput",
        "mcpServer/elicitation/request" => "mcpElicitation",
        _ => "request",
    }
}

fn decline_response_for_method(method: &str) -> JsonValue {
    match method {
        "item/commandExecution/requestApproval" => json!({ "decision": "decline" }),
        "item/fileChange/requestApproval" => json!({ "decision": "decline" }),
        "item/permissions/requestApproval" => json!({ "permissions": {}, "scope": "turn" }),
        "item/tool/requestUserInput" => json!({ "answers": {} }),
        "mcpServer/elicitation/request" => {
            json!({ "action": "decline", "content": null, "_meta": null })
        }
        _ => JsonValue::Null,
    }
}

fn normalize_dynamic_tool_result(value: Value) -> Result<JsonValue> {
    let value = rune_value_to_json(value)?;
    match value {
        JsonValue::String(text) => Ok(json!({
            "contentItems": [{ "type": "inputText", "text": text }],
            "success": true,
        })),
        JsonValue::Object(mut object) => {
            if let Some(content_items) = object.remove("content_items") {
                object.insert("contentItems".to_string(), content_items);
            }
            if !object.contains_key("contentItems") {
                object.insert(
                    "contentItems".to_string(),
                    JsonValue::Array(vec![json!({
                        "type": "inputText",
                        "text": serde_json::to_string(&JsonValue::Object(object.clone()))?,
                    })]),
                );
            }
            object
                .entry("success".to_string())
                .or_insert(JsonValue::Bool(true));
            Ok(JsonValue::Object(object))
        }
        JsonValue::Null => Ok(json!({ "contentItems": [], "success": true })),
        other => Ok(json!({
            "contentItems": [{
                "type": "inputText",
                "text": serde_json::to_string(&other)?,
            }],
            "success": true,
        })),
    }
}

fn failed_tool_result(message: String) -> JsonValue {
    json!({
        "contentItems": [{ "type": "inputText", "text": message }],
        "success": false,
    })
}

async fn await_rune_value(value: Value) -> Result<Value> {
    if value.type_hash() != RuneFuture::HASH {
        return Ok(value);
    }
    value
        .into_future()
        .map_err(anyhow::Error::from)?
        .await
        .into_result()
        .map_err(anyhow::Error::from)
}

struct RetryOptions {
    max_retries: usize,
    max_delay: Duration,
}

fn retry_options(options: JsonValue) -> RetryOptions {
    let max_retries = options
        .get("maxRetries")
        .or_else(|| options.get("max_retries"))
        .and_then(JsonValue::as_u64)
        .map(|value| value as usize)
        .unwrap_or(3);
    let max_delay = options
        .get("maxDelayMs")
        .or_else(|| options.get("max_delay_ms"))
        .and_then(JsonValue::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(RETRY_MAX_DELAY);
    RetryOptions {
        max_retries,
        max_delay,
    }
}

fn is_overload_error(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("overload")
        || message.contains("rate limit")
        || message.contains("429")
        || message.contains("temporarily unavailable")
}

pub(crate) fn object_without_keys(value: &Value, excluded_keys: &[&str]) -> Result<JsonValue> {
    if value.clone().into_unit().is_ok() {
        return Ok(JsonValue::Object(Map::new()));
    }
    let object = value
        .borrow_ref::<Object>()
        .map_err(|_| anyhow!("options must be an object"))?;
    let mut result = Map::new();
    for (key, value) in object.iter() {
        if excluded_keys.iter().any(|excluded| *excluded == key) {
            continue;
        }
        result.insert(key.to_string(), rune_value_to_json(value.clone())?);
    }
    Ok(JsonValue::Object(result))
}

pub(crate) async fn vm_result_from_future<T>(
    result: impl std::future::Future<Output = Result<T>>,
) -> VmResult<T> {
    match result.await {
        Ok(value) => VmResult::Ok(value),
        Err(err) => VmResult::panic(format!("{err:#}")),
    }
}

pub(crate) fn vm_result_from_result<T>(result: Result<T>) -> VmResult<T> {
    match result {
        Ok(value) => VmResult::Ok(value),
        Err(err) => VmResult::panic(format!("{err:#}")),
    }
}

pub(crate) fn rune_value_to_json(value: Value) -> Result<JsonValue> {
    serde_json::to_value(&value).context("failed to convert Rune value to JSON")
}

pub(crate) fn json_value_to_rune_result(value: JsonValue) -> Result<Value> {
    serde_json::from_value::<Value>(value).context("failed to convert JSON value to Rune")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::Ordering;

    use pretty_assertions::assert_eq;
    use rune::runtime::Function;
    use rune::runtime::Value;
    use serde_json::json;
    use tempfile::TempDir;

    use super::ApprovalMode;
    use super::RegisteredDynamicTool;
    use super::ServerRequestReply;
    use super::WorkflowRuneAppServer;
    use super::normalize_dynamic_tool_result;
    use crate::rune_app_server::json_value_to_rune_result;
    use crate::rune_app_server::rune_value_to_json;

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn request_returns_result_and_queues_notifications() {
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
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
    *'"method":"test/echo"'*)
      printf '%s\n' '{"method":"test/notification","params":{"queued":true}}'
      printf '%s\n' '{"id":"rune-request-1","result":{"ok":true}}'
      ;;
  esac
done
"#,
            )
            .expect("fake server");
            let mut permissions = fs::metadata(&server_path).expect("metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&server_path, permissions).expect("chmod");

            let client = WorkflowRuneAppServer::new(
                server_path.display().to_string(),
                temp.path().display().to_string(),
            );
            let response = client
                .request_json("test/echo", json!({ "value": 1 }))
                .await
                .expect("request");
            let notification = client.next_notification_json().await.expect("notification");

            assert_eq!(response, json!({ "ok": true }));
            assert_eq!(
                notification,
                json!({ "method": "test/notification", "params": { "queued": true } })
            );
            client.close_json().await.expect("close");
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[cfg(unix)]
    async fn concurrent_requests_accept_out_of_order_responses() {
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
    *'"method":"test/slow"'*)
      (sleep 0.2; printf '%s\n' '{"id":"rune-request-1","result":{"order":"slow"}}') &
      ;;
    *'"method":"test/fast"'*)
      printf '%s\n' '{"id":"rune-request-2","result":{"order":"fast"}}'
      ;;
  esac
done
"#,
                )
                .expect("fake server");
                let mut permissions = fs::metadata(&server_path).expect("metadata").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&server_path, permissions).expect("chmod");

                let client = WorkflowRuneAppServer::new(
                    server_path.display().to_string(),
                    temp.path().display().to_string(),
                );
                let slow = client.request_json("test/slow", json!({}));
                let fast = client.request_json("test/fast", json!({}));
                let (slow, fast) = tokio::join!(slow, fast);

                assert_eq!(slow.expect("slow response"), json!({ "order": "slow" }));
                assert_eq!(fast.expect("fast response"), json!({ "order": "fast" }));
                client.close_json().await.expect("close");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_tool_call_awaits_async_handler() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = WorkflowRuneAppServer::new("codex".to_string(), ".".to_string());
                client
                    .register_dynamic_tool(RegisteredDynamicTool {
                        namespace: None,
                        name: "lookup".to_string(),
                        handler: Function::new(|args: Value, call: Value| async move {
                            let args = rune_value_to_json(args).expect("args json");
                            let call = rune_value_to_json(call).expect("call json");
                            let query = args["query"].as_str().expect("query");
                            let tool = call["tool"].as_str().expect("tool");
                            let text = format!("{query}:{tool}");
                            json_value_to_rune_result(json!({
                                "contentItems": [{ "type": "inputText", "text": text }],
                            }))
                            .expect("tool result")
                        }),
                    })
                    .expect("register tool");

                let reply = client
                    .inner
                    .handle_server_request(json!({
                        "id": "tool-request-1",
                        "method": "item/tool/call",
                        "params": {
                            "tool": "lookup",
                            "arguments": { "query": "codex" },
                            "callId": "call-1",
                            "threadId": "thread-1",
                            "turnId": "turn-1",
                        },
                    }))
                    .await;

                match reply {
                    ServerRequestReply::Respond { id, result } => {
                        assert_eq!(id, json!("tool-request-1"));
                        assert_eq!(
                            result,
                            json!({
                                "contentItems": [{
                                    "type": "inputText",
                                    "text": "codex:lookup",
                                }],
                                "success": true,
                            })
                        );
                    }
                    ServerRequestReply::Reject { error, .. } => {
                        panic!("unexpected rejection: {}", error.message);
                    }
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn approval_request_awaits_async_handler() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let client = WorkflowRuneAppServer::new("codex".to_string(), ".".to_string());
                *client.inner.approvals.lock().expect("approval lock") =
                    ApprovalMode::Handler(Function::new(|payload: Value| async move {
                        let payload = rune_value_to_json(payload).expect("payload json");
                        json_value_to_rune_result(json!({
                            "decision": "approved",
                            "method": payload["method"],
                        }))
                        .expect("approval result")
                    }));

                let reply = client
                    .inner
                    .handle_server_request(json!({
                        "id": "approval-request-1",
                        "method": "item/commandExecution/requestApproval",
                        "params": { "command": ["echo", "ok"] },
                    }))
                    .await;

                match reply {
                    ServerRequestReply::Respond { id, result } => {
                        assert_eq!(id, json!("approval-request-1"));
                        assert_eq!(
                            result,
                            json!({
                                "decision": "approved",
                                "method": "item/commandExecution/requestApproval",
                            })
                        );
                    }
                    ServerRequestReply::Reject { error, .. } => {
                        panic!("unexpected rejection: {}", error.message);
                    }
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_dynamic_tool_handler_returns_failed_result() {
        let client = WorkflowRuneAppServer::new("codex".to_string(), ".".to_string());

        let reply = client
            .inner
            .handle_server_request(json!({
                "id": "tool-request-1",
                "method": "item/tool/call",
                "params": { "tool": "missing", "arguments": {} },
            }))
            .await;

        match reply {
            ServerRequestReply::Respond { result, .. } => {
                assert_eq!(result["success"], json!(false));
                assert_eq!(
                    result["contentItems"][0]["text"],
                    json!("missing Rune dynamic tool handler for `missing`")
                );
            }
            ServerRequestReply::Reject { error, .. } => {
                panic!("unexpected rejection: {}", error.message);
            }
        }
    }

    #[test]
    fn delegate_approvals_fail_fast_for_private_spawn() {
        let client = WorkflowRuneAppServer::new("codex".to_string(), ".".to_string());
        client
            .configure_json(json!({
                "connection": "spawn",
            }))
            .expect("configure");

        let err = client
            .set_approvals_value(json_value_to_rune_result(json!("delegate")).expect("mode"))
            .expect_err("delegate should fail for private spawn");

        assert_eq!(
            err.to_string(),
            "ctx.appServer.setApprovals(\"delegate\") requires an existing app-server connection"
        );
    }

    #[test]
    fn configure_after_connection_start_fails() {
        let client = WorkflowRuneAppServer::new("codex".to_string(), ".".to_string());
        client.inner.started.store(true, Ordering::SeqCst);

        let err = client
            .configure_json(json!({
                "connection": "spawn",
            }))
            .expect_err("configure should fail after start");

        assert_eq!(
            err.to_string(),
            "ctx.appServer.configure(...) must be called before the first app-server use"
        );
    }

    #[test]
    fn normalizes_dynamic_tool_result_shapes() {
        let text =
            normalize_dynamic_tool_result(json_value_to_rune_result(json!("hello")).unwrap())
                .expect("text result");
        let object = normalize_dynamic_tool_result(
            json_value_to_rune_result(json!({
                "contentItems": [{ "type": "inputText", "text": "ok" }],
                "success": false,
            }))
            .unwrap(),
        )
        .expect("object result");

        assert_eq!(
            text,
            json!({
                "contentItems": [{ "type": "inputText", "text": "hello" }],
                "success": true,
            })
        );
        assert_eq!(
            object,
            json!({
                "contentItems": [{ "type": "inputText", "text": "ok" }],
                "success": false,
            })
        );
    }
}
