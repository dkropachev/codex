use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_rmcp_client::Elicitation;
use codex_rmcp_client::ListToolsWithConnectorIdResult;
use codex_rmcp_client::SendElicitation;
use codex_uds::UnixStream;
use rmcp::model::CallToolResult;
use rmcp::model::InitializeRequestParams;
use rmcp::model::InitializeResult;
use rmcp::model::ListResourceTemplatesResult;
use rmcp::model::ListResourcesResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ReadResourceRequestParams;
use rmcp::model::ReadResourceResult;
use rmcp::model::RequestId;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::ReadHalf;
use tokio::io::WriteHalf;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time;
use tracing::debug;
use tracing::warn;

use super::START_LOCK_NAME;
use super::STARTUP_POLL;
use super::STARTUP_WAIT;
use super::control_socket_path;
use super::protocol::AcquireParams;
use super::protocol::AcquireResponse;
use super::protocol::BROKER_PROTOCOL_VERSION;
use super::protocol::CallToolParams;
use super::protocol::ClientLine;
use super::protocol::ElicitationClientResponse;
#[cfg(test)]
use super::protocol::EmptyResponse;
use super::protocol::HelloParams;
use super::protocol::HelloResponse;
use super::protocol::LeaseParams;
use super::protocol::ListResourceTemplatesParams;
use super::protocol::ListResourcesParams;
use super::protocol::ListToolsParams;
use super::protocol::METHOD_ACQUIRE;
use super::protocol::METHOD_CALL_TOOL;
use super::protocol::METHOD_HELLO;
use super::protocol::METHOD_LIST_RESOURCE_TEMPLATES;
use super::protocol::METHOD_LIST_RESOURCES;
use super::protocol::METHOD_LIST_TOOLS;
use super::protocol::METHOD_READ_RESOURCE;
use super::protocol::METHOD_RELEASE;
use super::protocol::ReadResourceParams;
use super::protocol::ReusableServerIdentity;
use super::protocol::ReusableServerLaunch;
use super::protocol::ServerLine;
use super::protocol::duration_to_millis;
use super::socket::prepare_broker_socket_directory;
use super::socket::prepare_broker_socket_path;
use super::socket::write_line;

/// Client-side lease for a brokered MCP server.
pub(crate) struct BrokerClient {
    lease_id: String,
    writer: Arc<Mutex<WriteHalf<UnixStream>>>,
    reader: Mutex<BufReader<ReadHalf<UnixStream>>>,
    request_lock: Mutex<()>,
    send_elicitation: SendElicitation,
    next_request_id: AtomicU64,
}

impl BrokerClient {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn acquire(
        codex_home: &Path,
        identity: ReusableServerIdentity,
        launch: ReusableServerLaunch,
        initialize_params: InitializeRequestParams,
        startup_timeout: Option<Duration>,
        send_elicitation: SendElicitation,
    ) -> Result<(Self, InitializeResult)> {
        let stream = connect_or_start(codex_home).await?;
        let (reader, writer) = tokio::io::split(stream);
        let mut client = Self {
            lease_id: String::new(),
            writer: Arc::new(Mutex::new(writer)),
            reader: Mutex::new(BufReader::new(reader)),
            request_lock: Mutex::new(()),
            send_elicitation,
            next_request_id: AtomicU64::new(1),
        };
        let _: HelloResponse = client
            .request(
                METHOD_HELLO,
                HelloParams {
                    version: BROKER_PROTOCOL_VERSION,
                },
            )
            .await?;
        let response: AcquireResponse = client
            .request(
                METHOD_ACQUIRE,
                AcquireParams {
                    identity,
                    launch,
                    initialize_params,
                    startup_timeout_ms: duration_to_millis(startup_timeout),
                },
            )
            .await?;
        client.lease_id = response.lease_id;
        Ok((client, response.initialize_result))
    }

    pub(crate) async fn list_tools_with_connector_ids(
        &self,
        params: Option<PaginatedRequestParams>,
        timeout: Option<Duration>,
    ) -> Result<ListToolsWithConnectorIdResult> {
        self.request(
            METHOD_LIST_TOOLS,
            ListToolsParams {
                lease_id: self.lease_id.clone(),
                params,
                timeout_ms: duration_to_millis(timeout),
            },
        )
        .await
    }

    pub(crate) async fn list_resources(
        &self,
        params: Option<PaginatedRequestParams>,
        timeout: Option<Duration>,
    ) -> Result<ListResourcesResult> {
        self.request(
            METHOD_LIST_RESOURCES,
            ListResourcesParams {
                lease_id: self.lease_id.clone(),
                params,
                timeout_ms: duration_to_millis(timeout),
            },
        )
        .await
    }

    pub(crate) async fn list_resource_templates(
        &self,
        params: Option<PaginatedRequestParams>,
        timeout: Option<Duration>,
    ) -> Result<ListResourceTemplatesResult> {
        self.request(
            METHOD_LIST_RESOURCE_TEMPLATES,
            ListResourceTemplatesParams {
                lease_id: self.lease_id.clone(),
                params,
                timeout_ms: duration_to_millis(timeout),
            },
        )
        .await
    }

    pub(crate) async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        timeout: Option<Duration>,
    ) -> Result<ReadResourceResult> {
        self.request(
            METHOD_READ_RESOURCE,
            ReadResourceParams {
                lease_id: self.lease_id.clone(),
                params,
                timeout_ms: duration_to_millis(timeout),
            },
        )
        .await
    }

    pub(crate) async fn call_tool(
        &self,
        name: String,
        arguments: Option<serde_json::Value>,
        meta: Option<serde_json::Value>,
        timeout: Option<Duration>,
    ) -> Result<CallToolResult> {
        self.request(
            METHOD_CALL_TOOL,
            CallToolParams {
                lease_id: self.lease_id.clone(),
                name,
                arguments,
                meta,
                timeout_ms: duration_to_millis(timeout),
            },
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn release(&mut self) -> Result<()> {
        if self.lease_id.is_empty() {
            return Ok(());
        }
        let _: EmptyResponse = self
            .request(
                METHOD_RELEASE,
                LeaseParams {
                    lease_id: self.lease_id.clone(),
                },
            )
            .await?;
        self.lease_id.clear();
        Ok(())
    }

    #[allow(
        clippy::await_holding_invalid_type,
        reason = "Broker RPCs are single-flight per client connection; the request and reader guards intentionally span request/response awaits."
    )]
    async fn request<P, R>(&self, method: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let _request_guard = self.request_lock.lock().await;
        let id = self.next_wire_id();
        let params = serde_json::to_value(params)?;
        write_line(
            &self.writer,
            &ClientLine::Request {
                id: id.clone(),
                method: method.to_string(),
                params,
            },
        )
        .await?;

        let mut reader = self.reader.lock().await;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line).await?;
            if bytes == 0 {
                return Err(anyhow!("MCP broker disconnected during {method}"));
            }
            match serde_json::from_str::<ServerLine>(&line)? {
                ServerLine::Response {
                    id: response_id,
                    result,
                    error,
                } if response_id == id => {
                    if let Some(error) = error {
                        return Err(anyhow!(error));
                    }
                    let result = result.ok_or_else(|| {
                        anyhow!("MCP broker response for {method} omitted result")
                    })?;
                    return serde_json::from_value(result).map_err(Into::into);
                }
                ServerLine::Response { .. } => {
                    return Err(anyhow!("MCP broker returned an out-of-order response"));
                }
                ServerLine::ElicitationRequest {
                    id,
                    request_id,
                    request,
                } => {
                    self.respond_to_elicitation(id, request_id, request).await?;
                }
            }
        }
    }

    async fn respond_to_elicitation(
        &self,
        id: String,
        request_id: RequestId,
        request: Elicitation,
    ) -> Result<()> {
        let response = (self.send_elicitation)(request_id, request).await;
        let response = match response {
            Ok(result) => ElicitationClientResponse {
                result: Some(result),
                error: None,
            },
            Err(error) => ElicitationClientResponse {
                result: None,
                error: Some(error.to_string()),
            },
        };
        write_line(
            &self.writer,
            &ClientLine::ElicitationResponse { id, response },
        )
        .await
    }

    fn next_wire_id(&self) -> String {
        self.next_request_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
    }
}

impl Drop for BrokerClient {
    fn drop(&mut self) {
        if self.lease_id.is_empty() {
            return;
        }
        let lease_id = self.lease_id.clone();
        let writer = Arc::clone(&self.writer);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = write_line(
                    &writer,
                    &ClientLine::Request {
                        id: "release".to_string(),
                        method: METHOD_RELEASE.to_string(),
                        params: serde_json::to_value(LeaseParams { lease_id })
                            .unwrap_or(serde_json::Value::Null),
                    },
                )
                .await;
            });
        }
    }
}

async fn connect_or_start(codex_home: &Path) -> Result<UnixStream> {
    let socket_path = control_socket_path(codex_home);
    if let Ok(stream) = connect_and_validate(&socket_path).await {
        return Ok(stream);
    }

    let parent = socket_path
        .parent()
        .ok_or_else(|| anyhow!("MCP broker socket path has no parent"))?;
    prepare_broker_socket_directory(&socket_path).await?;
    let lock_path = parent.join(START_LOCK_NAME);
    let lock = match StartLock::try_acquire(&lock_path) {
        Ok(lock) => Some(lock),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => None,
        Err(error) => return Err(error).context("failed to acquire MCP broker start lock"),
    };

    if let Ok(stream) = wait_for_broker(&socket_path, Duration::from_millis(500)).await {
        return Ok(stream);
    }

    if lock.is_some() {
        prepare_broker_socket_path(&socket_path).await?;
        spawn_broker_process(&socket_path).await?;
    }

    let stream = wait_for_broker(&socket_path, STARTUP_WAIT).await?;
    drop(lock);
    Ok(stream)
}

async fn wait_for_broker(socket_path: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = time::Instant::now() + timeout;
    loop {
        match connect_and_validate(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if time::Instant::now() >= deadline => return Err(error),
            Err(_) => time::sleep(STARTUP_POLL).await,
        }
    }
}

async fn connect_and_validate(socket_path: &Path) -> Result<UnixStream> {
    let stream = UnixStream::connect(socket_path).await?;
    Ok(stream)
}

async fn spawn_broker_process(socket_path: &Path) -> Result<()> {
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    let child = Command::new(current_exe)
        .arg("mcp-broker")
        .arg("--socket")
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn MCP broker process")?;
    debug!("spawned MCP broker process pid={:?}", child.id());
    Ok(())
}

struct StartLock {
    path: std::path::PathBuf,
}

impl StartLock {
    fn try_acquire(path: &Path) -> std::io::Result<Self> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for StartLock {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != ErrorKind::NotFound
        {
            warn!("failed to remove MCP broker start lock: {error}");
        }
    }
}
