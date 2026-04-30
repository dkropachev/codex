//! Per-user broker for opt-in reuse of local stdio MCP server processes.
//!
//! The broker speaks a private JSONL protocol over a Codex-home Unix domain
//! socket. Clients send fully resolved local stdio launch data; the broker
//! starts one MCP server for each reusable identity and multiplexes leases from
//! Codex processes owned by the same OS user.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_config::McpServerTransportConfig;
use codex_rmcp_client::Elicitation;
use codex_rmcp_client::ElicitationResponse;
use codex_rmcp_client::ResolvedStdioServerCommand;
use codex_rmcp_client::RmcpClient;
use codex_rmcp_client::SendElicitation;
use codex_rmcp_client::resolve_local_stdio_command;
use codex_uds::UnixListener;
use codex_uds::UnixStream;
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::future::Shared;
use rmcp::model::InitializeResult;
use rmcp::model::RequestId;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::time;
use tracing::warn;

use crate::runtime::McpRuntimeEnvironment;

mod client;
mod protocol;
mod socket;
#[cfg(test)]
mod tests;

pub(crate) use client::BrokerClient;
pub use protocol::MCP_PROTOCOL_VERSION;
pub use protocol::ReusableServerIdentity;
pub use protocol::ReusableServerLaunch;

use protocol::AcquireParams;
use protocol::AcquireResponse;
use protocol::BROKER_PROTOCOL_VERSION;
use protocol::CallToolParams;
use protocol::CallToolResponse;
use protocol::ClientLine;
use protocol::ElicitationClientResponse;
use protocol::EmptyResponse;
use protocol::HelloParams;
use protocol::HelloResponse;
use protocol::LeaseParams;
use protocol::ListResourceTemplatesParams;
use protocol::ListResourceTemplatesResponse;
use protocol::ListResourcesParams;
use protocol::ListResourcesResponse;
use protocol::ListToolsParams;
use protocol::ListToolsResponse;
use protocol::METHOD_ACQUIRE;
use protocol::METHOD_CALL_TOOL;
use protocol::METHOD_HELLO;
use protocol::METHOD_LIST_RESOURCE_TEMPLATES;
use protocol::METHOD_LIST_RESOURCES;
use protocol::METHOD_LIST_TOOLS;
use protocol::METHOD_READ_RESOURCE;
use protocol::METHOD_RELEASE;
use protocol::ReadResourceParams;
use protocol::ReadResourceResponse;
use protocol::ServerLine;
use protocol::millis_to_duration;
use socket::SocketFileGuard;
use socket::is_recoverable_accept_error;
use socket::prepare_broker_socket_path;
use socket::set_control_socket_permissions;
use socket::write_line;
use socket::write_response;

const SOCKET_DIR_NAME: &str = "mcp-broker";
const SOCKET_VERSION_DIR: &str = "v1";
const CONTROL_SOCKET_NAME: &str = "control.sock";
const START_LOCK_NAME: &str = "start.lock";
const STARTUP_WAIT: Duration = Duration::from_secs(5);
const STARTUP_POLL: Duration = Duration::from_millis(50);
const SERVER_IDLE_GRACE: Duration = Duration::from_secs(2);

/// Return the user-local broker control socket path for a Codex home.
pub fn control_socket_path(codex_home: &Path) -> PathBuf {
    codex_home
        .join(SOCKET_DIR_NAME)
        .join(SOCKET_VERSION_DIR)
        .join(CONTROL_SOCKET_NAME)
}

/// Run the broker accept loop at `socket_path`.
pub async fn run_broker(socket_path: PathBuf) -> Result<()> {
    prepare_broker_socket_path(&socket_path).await?;
    let mut listener = UnixListener::bind(&socket_path)
        .await
        .with_context(|| format!("failed to bind MCP broker socket {}", socket_path.display()))?;
    set_control_socket_permissions(&socket_path).await?;
    let _socket_guard = SocketFileGuard {
        socket_path: socket_path.clone(),
    };
    let state = Arc::new(BrokerState::default());

    loop {
        match listener.accept().await {
            Ok(stream) => {
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    handle_connection(state, stream).await;
                });
            }
            Err(error) if is_recoverable_accept_error(&error) => {
                warn!("recoverable MCP broker accept error: {error}");
            }
            Err(error) => return Err(error).context("MCP broker accept failed"),
        }
    }
}

pub(crate) fn reusable_stdio_identity(
    transport: &McpServerTransportConfig,
    runtime_environment: &McpRuntimeEnvironment,
) -> Result<Option<(ReusableServerIdentity, ReusableServerLaunch)>> {
    if runtime_environment.environment().is_remote() {
        return Ok(None);
    }

    let McpServerTransportConfig::Stdio {
        command,
        args,
        env,
        env_vars,
        cwd,
    } = transport
    else {
        return Ok(None);
    };

    let env = env.clone().map(|env| {
        env.into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect::<HashMap<_, _>>()
    });
    let resolved = resolve_local_stdio_command(
        OsString::from(command),
        args.iter().map(OsString::from).collect(),
        env,
        env_vars.clone(),
        cwd.clone(),
        runtime_environment.fallback_cwd(),
    )?;
    let launch = launch_from_resolved(resolved)?;
    let identity = ReusableServerIdentity {
        command: launch.command.clone(),
        args: launch.args.clone(),
        cwd: launch.cwd.clone(),
        env: launch.env.clone(),
        placement: "local".to_string(),
        protocol_version: MCP_PROTOCOL_VERSION.to_string(),
    };
    Ok(Some((identity, launch)))
}

fn launch_from_resolved(resolved: ResolvedStdioServerCommand) -> Result<ReusableServerLaunch> {
    let ResolvedStdioServerCommand {
        program,
        args,
        env,
        cwd,
    } = resolved;

    let command = os_string_to_string(program, "command")?;
    let args = args
        .into_iter()
        .map(|arg| os_string_to_string(arg, "argument"))
        .collect::<Result<Vec<_>>>()?;
    let cwd = cwd
        .to_str()
        .ok_or_else(|| anyhow!("MCP stdio cwd must be valid UTF-8 for process reuse"))?
        .to_string();
    let env = env
        .into_iter()
        .map(|(key, value)| {
            Ok((
                os_string_to_string(key, "environment variable name")?,
                os_string_to_string(value, "environment variable value")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;

    Ok(ReusableServerLaunch {
        command,
        args,
        cwd,
        env,
    })
}

fn os_string_to_string(value: OsString, label: &str) -> Result<String> {
    value
        .into_string()
        .map_err(|_| anyhow!("MCP stdio {label} must be valid UTF-8 for process reuse"))
}

#[derive(Default)]
struct BrokerState {
    servers: Mutex<HashMap<ReusableServerIdentity, Arc<SharedServer>>>,
}

struct SharedServer {
    identity: ReusableServerIdentity,
    lease_count: AtomicUsize,
    startup: Shared<BoxFuture<'static, Result<Arc<ServerRuntime>, String>>>,
    operation_lock: Mutex<()>,
    elicitation_router: Arc<ElicitationRouter>,
}

struct ServerRuntime {
    client: Arc<RmcpClient>,
    initialize_result: InitializeResult,
}

#[derive(Default)]
struct ElicitationRouter {
    active: Mutex<Option<Arc<Connection>>>,
}

impl ElicitationRouter {
    async fn send(
        &self,
        request_id: RequestId,
        request: Elicitation,
    ) -> Result<ElicitationResponse> {
        let active = self.active.lock().await.clone();
        let Some(connection) = active else {
            return Ok(cancel_elicitation_response());
        };
        connection.send_elicitation(request_id, request).await
    }

    async fn set_active(&self, connection: Option<Arc<Connection>>) {
        *self.active.lock().await = connection;
    }
}

struct Connection {
    state: Arc<BrokerState>,
    writer: Arc<Mutex<WriteHalf<UnixStream>>>,
    leases: Mutex<HashMap<String, ReusableServerIdentity>>,
    pending_elicitations: Mutex<HashMap<String, oneshot::Sender<Result<ElicitationResponse>>>>,
    next_elicitation_id: AtomicU64,
    closed: AtomicBool,
}

impl Connection {
    async fn send_elicitation(
        &self,
        request_id: RequestId,
        request: Elicitation,
    ) -> Result<ElicitationResponse> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(cancel_elicitation_response());
        }

        let id = format!(
            "e{}",
            self.next_elicitation_id.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        self.pending_elicitations
            .lock()
            .await
            .insert(id.clone(), tx);
        let write_result = write_line(
            &self.writer,
            &ServerLine::ElicitationRequest {
                id: id.clone(),
                request_id,
                request,
            },
        )
        .await;

        if let Err(error) = write_result {
            self.pending_elicitations.lock().await.remove(&id);
            warn!("failed to forward MCP elicitation through broker: {error}");
            return Ok(cancel_elicitation_response());
        }

        match rx.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(error),
            Err(_) => Ok(cancel_elicitation_response()),
        }
    }
}

async fn handle_connection(state: Arc<BrokerState>, stream: UnixStream) {
    let (reader, writer) = tokio::io::split(stream);
    let connection = Arc::new(Connection {
        state,
        writer: Arc::new(Mutex::new(writer)),
        leases: Mutex::new(HashMap::new()),
        pending_elicitations: Mutex::new(HashMap::new()),
        next_elicitation_id: AtomicU64::new(1),
        closed: AtomicBool::new(false),
    });
    let mut reader = BufReader::new(reader);

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => match serde_json::from_str::<ClientLine>(&line) {
                Ok(ClientLine::Request { id, method, params }) => {
                    let connection = Arc::clone(&connection);
                    tokio::spawn(async move {
                        handle_request(connection, id, method, params).await;
                    });
                }
                Ok(ClientLine::ElicitationResponse { id, response }) => {
                    complete_elicitation(&connection, id, response).await;
                }
                Err(error) => warn!("invalid MCP broker client line: {error}"),
            },
            Err(error) => {
                warn!("MCP broker connection read failed: {error}");
                break;
            }
        }
    }

    connection.closed.store(true, Ordering::Release);
    let pending = std::mem::take(&mut *connection.pending_elicitations.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Ok(cancel_elicitation_response()));
    }
    release_all_connection_leases(&connection).await;
}

async fn handle_request(
    connection: Arc<Connection>,
    id: String,
    method: String,
    params: serde_json::Value,
) {
    let result = match method.as_str() {
        METHOD_HELLO => handle_hello(params).await,
        METHOD_ACQUIRE => handle_acquire(Arc::clone(&connection), params).await,
        METHOD_RELEASE => handle_release(Arc::clone(&connection), params).await,
        METHOD_LIST_TOOLS => handle_list_tools(Arc::clone(&connection), params).await,
        METHOD_LIST_RESOURCES => handle_list_resources(Arc::clone(&connection), params).await,
        METHOD_LIST_RESOURCE_TEMPLATES => {
            handle_list_resource_templates(Arc::clone(&connection), params).await
        }
        METHOD_READ_RESOURCE => handle_read_resource(Arc::clone(&connection), params).await,
        METHOD_CALL_TOOL => handle_call_tool(Arc::clone(&connection), params).await,
        _ => Err(anyhow!("unknown MCP broker method `{method}`")),
    };

    match result {
        Ok(result) => {
            let _ = write_response(&connection.writer, id, Some(result), None).await;
        }
        Err(error) => {
            let _ = write_response(&connection.writer, id, None, Some(error.to_string())).await;
        }
    }
}

async fn handle_hello(params: serde_json::Value) -> Result<serde_json::Value> {
    let params: HelloParams = serde_json::from_value(params)?;
    if params.version != BROKER_PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported MCP broker protocol version {}; expected {}",
            params.version,
            BROKER_PROTOCOL_VERSION
        ));
    }
    serde_json::to_value(HelloResponse {
        version: BROKER_PROTOCOL_VERSION,
    })
    .map_err(Into::into)
}

async fn handle_acquire(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: AcquireParams = serde_json::from_value(params)?;
    let shared = shared_server_for_acquire(&connection.state, &params).await;
    let runtime = match shared.startup.clone().await {
        Ok(runtime) => runtime,
        Err(error) => {
            remove_failed_server(&connection.state, &shared.identity).await;
            return Err(anyhow!(error));
        }
    };
    if connection.closed.load(Ordering::Acquire) {
        return Err(anyhow!("MCP broker connection closed during acquire"));
    }
    shared.lease_count.fetch_add(1, Ordering::AcqRel);
    let lease_id = format!(
        "{}-{}",
        std::process::id(),
        NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)
    );
    connection
        .leases
        .lock()
        .await
        .insert(lease_id.clone(), shared.identity.clone());
    if connection.closed.load(Ordering::Acquire) {
        release_connection_lease(&connection, &lease_id).await?;
        return Err(anyhow!("MCP broker connection closed during acquire"));
    }
    serde_json::to_value(AcquireResponse {
        lease_id,
        initialize_result: runtime.initialize_result.clone(),
    })
    .map_err(Into::into)
}

static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

async fn shared_server_for_acquire(
    state: &BrokerState,
    params: &AcquireParams,
) -> Arc<SharedServer> {
    let mut servers = state.servers.lock().await;
    if let Some(shared) = servers.get(&params.identity) {
        return Arc::clone(shared);
    }

    let shared = Arc::new(new_shared_server(params));
    servers.insert(params.identity.clone(), Arc::clone(&shared));
    shared
}

fn new_shared_server(params: &AcquireParams) -> SharedServer {
    let identity = params.identity.clone();
    let launch = params.launch.clone();
    let initialize_params = params.initialize_params.clone();
    let startup_timeout = millis_to_duration(params.startup_timeout_ms);
    let elicitation_router = Arc::new(ElicitationRouter::default());
    let router = Arc::clone(&elicitation_router);
    let startup = async move {
        let resolved = resolved_command_from_launch(launch).map_err(|error| error.to_string())?;
        let client = RmcpClient::new_resolved_stdio_client(resolved)
            .await
            .map_err(|error| error.to_string())?;
        let client = Arc::new(client);
        let send_elicitation: SendElicitation = Box::new(move |request_id, request| {
            let router = Arc::clone(&router);
            async move { router.send(request_id, request).await }.boxed()
        });
        let initialize_result = client
            .initialize(initialize_params, startup_timeout, send_elicitation)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(ServerRuntime {
            client,
            initialize_result,
        }))
    }
    .boxed()
    .shared();

    SharedServer {
        identity,
        lease_count: AtomicUsize::new(0),
        startup,
        operation_lock: Mutex::new(()),
        elicitation_router,
    }
}

fn resolved_command_from_launch(
    launch: ReusableServerLaunch,
) -> Result<ResolvedStdioServerCommand> {
    Ok(ResolvedStdioServerCommand {
        program: OsString::from(launch.command),
        args: launch.args.into_iter().map(OsString::from).collect(),
        env: launch
            .env
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        cwd: PathBuf::from(launch.cwd),
    })
}

async fn remove_failed_server(state: &BrokerState, identity: &ReusableServerIdentity) {
    state.servers.lock().await.remove(identity);
}

async fn handle_release(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: LeaseParams = serde_json::from_value(params)?;
    release_connection_lease(&connection, &params.lease_id).await?;
    serde_json::to_value(EmptyResponse {}).map_err(Into::into)
}

async fn handle_list_tools(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: ListToolsParams = serde_json::from_value(params)?;
    let request_params = params.params.clone();
    let timeout = millis_to_duration(params.timeout_ms);
    let response: ListToolsResponse =
        with_shared_server(&connection, &params.lease_id, |runtime| {
            async move {
                runtime
                    .client
                    .list_tools_with_connector_ids(request_params, timeout)
                    .await
            }
            .boxed()
        })
        .await?;
    serde_json::to_value(response).map_err(Into::into)
}

async fn handle_list_resources(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: ListResourcesParams = serde_json::from_value(params)?;
    let request_params = params.params.clone();
    let timeout = millis_to_duration(params.timeout_ms);
    let response: ListResourcesResponse =
        with_shared_server(&connection, &params.lease_id, |runtime| {
            async move { runtime.client.list_resources(request_params, timeout).await }.boxed()
        })
        .await?;
    serde_json::to_value(response).map_err(Into::into)
}

async fn handle_list_resource_templates(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: ListResourceTemplatesParams = serde_json::from_value(params)?;
    let request_params = params.params.clone();
    let timeout = millis_to_duration(params.timeout_ms);
    let response: ListResourceTemplatesResponse =
        with_shared_server(&connection, &params.lease_id, |runtime| {
            async move {
                runtime
                    .client
                    .list_resource_templates(request_params, timeout)
                    .await
            }
            .boxed()
        })
        .await?;
    serde_json::to_value(response).map_err(Into::into)
}

async fn handle_read_resource(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: ReadResourceParams = serde_json::from_value(params)?;
    let request_params = params.params.clone();
    let timeout = millis_to_duration(params.timeout_ms);
    let response: ReadResourceResponse =
        with_shared_server(&connection, &params.lease_id, |runtime| {
            async move { runtime.client.read_resource(request_params, timeout).await }.boxed()
        })
        .await?;
    serde_json::to_value(response).map_err(Into::into)
}

async fn handle_call_tool(
    connection: Arc<Connection>,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let params: CallToolParams = serde_json::from_value(params)?;
    let response: CallToolResponse = with_shared_server(&connection, &params.lease_id, |runtime| {
        let name = params.name.clone();
        let arguments = params.arguments.clone();
        let meta = params.meta.clone();
        let timeout = millis_to_duration(params.timeout_ms);
        async move {
            runtime
                .client
                .call_tool(name, arguments, meta, timeout)
                .await
        }
        .boxed()
    })
    .await?;
    serde_json::to_value(response).map_err(Into::into)
}

#[allow(
    clippy::await_holding_invalid_type,
    reason = "v1 broker serializes operations per shared MCP server so elicitation responses route to the active caller."
)]
async fn with_shared_server<T>(
    connection: &Arc<Connection>,
    lease_id: &str,
    operation: impl FnOnce(Arc<ServerRuntime>) -> BoxFuture<'static, Result<T>>,
) -> Result<T> {
    let identity = connection
        .leases
        .lock()
        .await
        .get(lease_id)
        .cloned()
        .ok_or_else(|| anyhow!("unknown MCP broker lease `{lease_id}`"))?;
    let shared = connection
        .state
        .servers
        .lock()
        .await
        .get(&identity)
        .cloned()
        .ok_or_else(|| anyhow!("MCP broker lease `{lease_id}` no longer has a server"))?;
    let runtime = shared
        .startup
        .clone()
        .await
        .map_err(|error| anyhow!(error))?;
    let _operation_guard = shared.operation_lock.lock().await;
    shared
        .elicitation_router
        .set_active(Some(Arc::clone(connection)))
        .await;
    let result = operation(runtime).await;
    shared.elicitation_router.set_active(None).await;
    result
}

async fn complete_elicitation(
    connection: &Connection,
    id: String,
    response: ElicitationClientResponse,
) {
    let sender = connection.pending_elicitations.lock().await.remove(&id);
    let Some(sender) = sender else {
        return;
    };
    let result = match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (_, Some(error)) => Err(anyhow!(error)),
        (None, None) => Ok(cancel_elicitation_response()),
    };
    let _ = sender.send(result);
}

async fn release_all_connection_leases(connection: &Connection) {
    let lease_ids = connection
        .leases
        .lock()
        .await
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for lease_id in lease_ids {
        if let Err(error) = release_connection_lease(connection, &lease_id).await {
            warn!("failed to release MCP broker lease {lease_id}: {error}");
        }
    }
}

async fn release_connection_lease(connection: &Connection, lease_id: &str) -> Result<()> {
    let Some(identity) = connection.leases.lock().await.remove(lease_id) else {
        return Ok(());
    };
    let shared = connection
        .state
        .servers
        .lock()
        .await
        .get(&identity)
        .cloned();
    if let Some(shared) = shared
        && shared.lease_count.fetch_sub(1, Ordering::AcqRel) == 1
    {
        schedule_idle_server_removal(Arc::clone(&connection.state), identity, shared);
    }
    Ok(())
}

fn schedule_idle_server_removal(
    state: Arc<BrokerState>,
    identity: ReusableServerIdentity,
    shared: Arc<SharedServer>,
) {
    tokio::spawn(async move {
        time::sleep(SERVER_IDLE_GRACE).await;
        if shared.lease_count.load(Ordering::Acquire) != 0 {
            return;
        }
        let mut servers = state.servers.lock().await;
        if servers
            .get(&identity)
            .is_some_and(|current| Arc::ptr_eq(current, &shared))
            && shared.lease_count.load(Ordering::Acquire) == 0
        {
            servers.remove(&identity);
        }
    });
}

fn cancel_elicitation_response() -> ElicitationResponse {
    ElicitationResponse {
        action: codex_rmcp_client::ElicitationAction::Cancel,
        content: None,
        meta: None,
    }
}
