use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::AtomicUsize;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use anyhow::anyhow;
use codex_config::McpServerConfig;
use codex_config::McpServerProcessReuseScope;
use codex_config::McpServerTransportConfig;
use codex_exec_server::Environment;
#[cfg(unix)]
use codex_rmcp_client::ElicitationAction;
#[cfg(unix)]
use codex_rmcp_client::ElicitationResponse;
#[cfg(unix)]
use codex_rmcp_client::SendElicitation;
use codex_uds::UnixListener;
use codex_uds::UnixStream;
#[cfg(unix)]
use futures::FutureExt;
use pretty_assertions::assert_eq;
#[cfg(unix)]
use rmcp::model::ClientCapabilities;
#[cfg(unix)]
use rmcp::model::ElicitationCapability;
#[cfg(unix)]
use rmcp::model::FormElicitationCapability;
#[cfg(unix)]
use rmcp::model::Implementation;
#[cfg(unix)]
use rmcp::model::InitializeRequestParams;
#[cfg(unix)]
use rmcp::model::ProtocolVersion;
#[cfg(unix)]
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::sync::Mutex;
use tokio::time;
use tokio::time::timeout;

#[cfg(unix)]
use super::BrokerClient;
use super::ReusableServerIdentity;
#[cfg(unix)]
use super::SERVER_IDLE_GRACE;
#[cfg(unix)]
use super::control_socket_path;
use super::protocol::ClientLine;
use super::protocol::HelloParams;
use super::protocol::METHOD_HELLO;
use super::protocol::ServerLine;
use super::reusable_stdio_identity;
use super::run_broker;
use super::socket::write_line;
use crate::runtime::McpRuntimeEnvironment;

fn runtime(fallback_cwd: &Path) -> McpRuntimeEnvironment {
    McpRuntimeEnvironment::new(
        Arc::new(Environment::default_for_tests()),
        fallback_cwd.to_path_buf(),
    )
}

fn stdio_transport(
    command: &str,
    args: &[&str],
    env: &[(&str, &str)],
    cwd: &Path,
) -> McpServerTransportConfig {
    McpServerTransportConfig::Stdio {
        command: command.to_string(),
        args: args.iter().map(ToString::to_string).collect(),
        env: Some(
            env.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        ),
        env_vars: Vec::new(),
        cwd: Some(cwd.to_path_buf()),
    }
}

fn server_config(
    transport: McpServerTransportConfig,
    process_reuse_scope: McpServerProcessReuseScope,
) -> McpServerConfig {
    McpServerConfig {
        transport,
        process_reuse_scope,
        experimental_environment: None,
        enabled: true,
        required: false,
        supports_parallel_tool_calls: false,
        disabled_reason: None,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        default_tools_approval_mode: None,
        enabled_tools: None,
        disabled_tools: None,
        scopes: None,
        oauth_resource: None,
        tools: std::collections::HashMap::new(),
    }
}

fn identity_for(transport: &McpServerTransportConfig, cwd: &Path) -> ReusableServerIdentity {
    identity_for_scope(transport, cwd, McpServerProcessReuseScope::Cwd, &[".git"])
}

fn identity_for_scope(
    transport: &McpServerTransportConfig,
    cwd: &Path,
    scope: McpServerProcessReuseScope,
    project_root_markers: &[&str],
) -> ReusableServerIdentity {
    let config = server_config(transport.clone(), scope);
    let runtime = runtime(cwd).with_project_root_markers(
        project_root_markers
            .iter()
            .map(ToString::to_string)
            .collect(),
    );
    reusable_stdio_identity(&config, &runtime)
        .expect("identity should resolve")
        .expect("stdio transport should be reusable")
        .0
}

#[cfg(unix)]
fn initialize_params() -> InitializeRequestParams {
    InitializeRequestParams {
        meta: None,
        capabilities: ClientCapabilities {
            experimental: None,
            extensions: None,
            roots: None,
            sampling: None,
            elicitation: Some(ElicitationCapability {
                form: Some(FormElicitationCapability {
                    schema_validation: None,
                }),
                url: None,
            }),
            tasks: None,
        },
        client_info: Implementation {
            name: "codex-mcp-broker-test".to_string(),
            version: "0.0.0".to_string(),
            title: None,
            description: None,
            icons: None,
            website_url: None,
        },
        protocol_version: ProtocolVersion::V_2025_06_18,
    }
}

#[cfg(unix)]
fn cancel_elicitation_sender() -> SendElicitation {
    Box::new(|_, _| {
        async {
            Ok(ElicitationResponse {
                action: ElicitationAction::Cancel,
                content: None,
                meta: None,
            })
        }
        .boxed()
    })
}

#[cfg(unix)]
fn counting_elicitation_sender(count: Arc<AtomicUsize>) -> SendElicitation {
    Box::new(move |_, _| {
        let count = Arc::clone(&count);
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(ElicitationResponse {
                action: ElicitationAction::Accept,
                content: Some(json!({ "confirmed": true })),
                meta: None,
            })
        }
        .boxed()
    })
}

#[cfg(unix)]
fn startup_log_entries(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(str::to_string)
        .collect())
}

#[cfg(unix)]
fn stdio_test_server_transport(command: PathBuf, startup_log: &Path) -> McpServerTransportConfig {
    McpServerTransportConfig::Stdio {
        command: command.to_string_lossy().to_string(),
        args: Vec::new(),
        env: Some(
            [(
                "MCP_TEST_STARTUP_LOG".to_string(),
                startup_log.to_string_lossy().to_string(),
            )]
            .into_iter()
            .collect(),
        ),
        env_vars: Vec::new(),
        cwd: startup_log.parent().map(Path::to_path_buf),
    }
}

#[cfg(unix)]
fn write_stdio_test_server(dir: &Path) -> Result<PathBuf> {
    const SCRIPT: &str = r#"#!/bin/sh
printf '%s\n' "$$" >> "$MCP_TEST_STARTUP_LOG"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":[[:space:]]*\([^,}]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false},"resources":{}},"serverInfo":{"name":"broker-test","version":"0.0.0"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ask","description":"Ask for confirmation.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]}}\n' "$id"
      ;;
    *'"method":"resources/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resources":[]}}\n' "$id"
      ;;
    *'"method":"resources/templates/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"resourceTemplates":[]}}\n' "$id"
      ;;
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":"elicitation-1","method":"elicitation/create","params":{"message":"Confirm?","requestedSchema":{"type":"object","properties":{"confirmed":{"type":"boolean"}},"required":["confirmed"],"additionalProperties":false}}}\n'
      IFS= read -r elicitation_response
      if [ -n "$MCP_TEST_ELICITATION_LOG" ]; then
        printf '%s\n' "$elicitation_response" >> "$MCP_TEST_ELICITATION_LOG"
      fi
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[],"structuredContent":{"elicited":true},"isError":false}}\n' "$id"
      ;;
  esac
done
"#;
    let path = dir.join("stdio-mcp-test-server.sh");
    std::fs::write(&path, SCRIPT)?;
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions)?;
    Ok(path)
}

#[tokio::test]
async fn reusable_identity_ignores_server_alias() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let transport = stdio_transport("server-command", &["--mode", "test"], &[], temp.path());

    let docs_alias = identity_for(&transport, temp.path());
    let other_alias = identity_for(&transport, temp.path());

    assert_eq!(docs_alias, other_alias);
}

#[tokio::test]
async fn reusable_identity_changes_for_args_cwd_env_and_command() {
    let first = tempfile::TempDir::new().expect("first tempdir");
    let second = tempfile::TempDir::new().expect("second tempdir");
    let base = stdio_transport(
        "server-command",
        &["one"],
        &[("TOKEN", "alpha")],
        first.path(),
    );
    let base_identity = identity_for(&base, first.path());

    let changed_args = stdio_transport(
        "server-command",
        &["two"],
        &[("TOKEN", "alpha")],
        first.path(),
    );
    let changed_cwd = stdio_transport(
        "server-command",
        &["one"],
        &[("TOKEN", "alpha")],
        second.path(),
    );
    let changed_env = stdio_transport(
        "server-command",
        &["one"],
        &[("TOKEN", "bravo")],
        first.path(),
    );
    let changed_command = stdio_transport(
        "other-command",
        &["one"],
        &[("TOKEN", "alpha")],
        first.path(),
    );

    assert_ne!(base_identity, identity_for(&changed_args, first.path()));
    assert_ne!(base_identity, identity_for(&changed_cwd, first.path()));
    assert_ne!(base_identity, identity_for(&changed_env, first.path()));
    assert_ne!(base_identity, identity_for(&changed_command, first.path()));
}

#[tokio::test]
async fn reusable_identity_uses_fallback_cwd_when_config_omits_cwd() {
    let first = tempfile::TempDir::new().expect("first tempdir");
    let second = tempfile::TempDir::new().expect("second tempdir");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    assert_ne!(
        identity_for(&transport, first.path()),
        identity_for(&transport, second.path())
    );
}

#[tokio::test]
async fn reusable_identity_none_scope_disables_broker_reuse() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let transport = stdio_transport("server-command", &[], &[], temp.path());
    let config = server_config(transport, McpServerProcessReuseScope::None);

    assert!(
        reusable_stdio_identity(&config, &runtime(temp.path()))
            .expect("identity resolution should not fail")
            .is_none()
    );
}

#[tokio::test]
async fn reusable_identity_project_scope_uses_detected_project_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let project = temp.path().join("project");
    let first = project.join("first");
    let second = project.join("second");
    std::fs::create_dir_all(&first).expect("create first dir");
    std::fs::create_dir_all(&second).expect("create second dir");
    std::fs::write(project.join(".project-root"), "").expect("write marker");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    let first_identity = identity_for_scope(
        &transport,
        &first,
        McpServerProcessReuseScope::Project,
        &[".project-root"],
    );
    let second_identity = identity_for_scope(
        &transport,
        &second,
        McpServerProcessReuseScope::Project,
        &[".project-root"],
    );

    assert_eq!(first_identity, second_identity);
    assert_eq!(
        first_identity.reuse_partition_key,
        format!("project:{}", project.display())
    );
}

#[tokio::test]
async fn reusable_identity_project_scope_falls_back_to_cwd() {
    let first = tempfile::TempDir::new().expect("first tempdir");
    let second = tempfile::TempDir::new().expect("second tempdir");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    let first_identity = identity_for_scope(
        &transport,
        first.path(),
        McpServerProcessReuseScope::Project,
        &[".project-root"],
    );
    let second_identity = identity_for_scope(
        &transport,
        second.path(),
        McpServerProcessReuseScope::Project,
        &[".project-root"],
    );

    assert_ne!(first_identity, second_identity);
    assert!(first_identity.reuse_partition_key.starts_with("cwd:"));
}

#[tokio::test]
async fn reusable_identity_repo_scope_uses_git_root() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let repo = temp.path().join("repo");
    let first = repo.join("first");
    let second = repo.join("second");
    std::fs::create_dir_all(repo.join(".git")).expect("create git dir");
    std::fs::create_dir_all(&first).expect("create first dir");
    std::fs::create_dir_all(&second).expect("create second dir");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    let first_identity = identity_for_scope(
        &transport,
        &first,
        McpServerProcessReuseScope::Repo,
        &[".project-root"],
    );
    let second_identity = identity_for_scope(
        &transport,
        &second,
        McpServerProcessReuseScope::Repo,
        &[".project-root"],
    );

    assert_eq!(first_identity, second_identity);
    assert_eq!(
        first_identity.reuse_partition_key,
        format!("repo:{}", repo.display())
    );
}

#[tokio::test]
async fn reusable_identity_repo_scope_falls_back_to_project_then_cwd() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let project = temp.path().join("project");
    let first = project.join("first");
    let second = project.join("second");
    std::fs::create_dir_all(&first).expect("create first dir");
    std::fs::create_dir_all(&second).expect("create second dir");
    std::fs::write(project.join(".project-root"), "").expect("write marker");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };

    let first_project_identity = identity_for_scope(
        &transport,
        &first,
        McpServerProcessReuseScope::Repo,
        &[".project-root"],
    );
    let second_project_identity = identity_for_scope(
        &transport,
        &second,
        McpServerProcessReuseScope::Repo,
        &[".project-root"],
    );

    assert_eq!(first_project_identity, second_project_identity);
    assert_eq!(
        first_project_identity.reuse_partition_key,
        format!("project:{}", project.display())
    );

    let first_cwd_identity = identity_for_scope(
        &transport,
        &first,
        McpServerProcessReuseScope::Repo,
        &["absent"],
    );
    let second_cwd_identity = identity_for_scope(
        &transport,
        &second,
        McpServerProcessReuseScope::Repo,
        &["absent"],
    );

    assert_ne!(first_cwd_identity, second_cwd_identity);
    assert!(first_cwd_identity.reuse_partition_key.starts_with("cwd:"));
}

#[tokio::test]
async fn reusable_identity_user_scope_uses_user_partition() {
    let launch = tempfile::TempDir::new().expect("launch tempdir");
    let first = tempfile::TempDir::new().expect("first tempdir");
    let second = tempfile::TempDir::new().expect("second tempdir");
    let transport = stdio_transport("server-command", &[], &[], launch.path());

    let first_identity = identity_for_scope(
        &transport,
        first.path(),
        McpServerProcessReuseScope::User,
        &[".git"],
    );
    let second_identity = identity_for_scope(
        &transport,
        second.path(),
        McpServerProcessReuseScope::User,
        &[".git"],
    );

    assert_eq!(first_identity, second_identity);
    assert_eq!(first_identity.reuse_partition_key, "user");
}

#[tokio::test]
async fn reusable_identity_user_scope_requires_explicit_absolute_cwd() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let transport = McpServerTransportConfig::Stdio {
        command: "server-command".to_string(),
        args: Vec::new(),
        env: None,
        env_vars: Vec::new(),
        cwd: None,
    };
    let config = server_config(transport, McpServerProcessReuseScope::User);

    let error = reusable_stdio_identity(&config, &runtime(temp.path()))
        .expect_err("user scope without cwd should be rejected");
    assert!(
        error.to_string().contains("requires an explicit stdio cwd"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn broker_rejects_protocol_version_mismatch() -> Result<()> {
    let temp = tempfile::TempDir::new()?;
    let socket_path = temp.path().join("broker.sock");
    let handle = tokio::spawn(run_broker(socket_path.clone()));
    let stream = wait_for_test_socket(&socket_path).await?;
    let (reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));

    write_line(
        &writer,
        &ClientLine::Request {
            id: "1".to_string(),
            method: METHOD_HELLO.to_string(),
            params: serde_json::to_value(HelloParams { version: 999 })?,
        },
    )
    .await?;

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let response = serde_json::from_str::<ServerLine>(&line)?;
    let ServerLine::Response { error, .. } = response else {
        panic!("expected response");
    };
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("unsupported MCP broker protocol version"))
    );

    handle.abort();
    Ok(())
}

#[tokio::test]
async fn broker_cleans_up_stale_socket_path() -> Result<()> {
    let temp = tempfile::TempDir::new()?;
    let socket_path = temp.path().join("stale.sock");
    {
        let _listener = UnixListener::bind(&socket_path).await?;
    }

    let handle = tokio::spawn(run_broker(socket_path.clone()));
    let _stream = wait_for_test_socket(&socket_path).await?;
    handle.abort();
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn broker_reuses_stdio_process_until_last_release() -> Result<()> {
    let temp = tempfile::TempDir::new()?;
    let codex_home = temp.path().join("codex-home");
    tokio::fs::create_dir(&codex_home).await?;
    let socket_path = control_socket_path(&codex_home);
    let handle = tokio::spawn(run_broker(socket_path.clone()));
    let stream = wait_for_test_socket(&socket_path).await?;
    drop(stream);

    let startup_log = temp.path().join("startups.log");
    let server = write_stdio_test_server(temp.path())?;
    let transport = stdio_test_server_transport(server, &startup_log);
    let config = server_config(transport, McpServerProcessReuseScope::Cwd);
    let (identity, launch) = reusable_stdio_identity(&config, &runtime(temp.path()))?
        .expect("local stdio transport should be reusable");

    let (mut first, _) = BrokerClient::acquire(
        &codex_home,
        identity.clone(),
        launch.clone(),
        initialize_params(),
        Some(Duration::from_secs(5)),
        cancel_elicitation_sender(),
    )
    .await?;
    let (mut second, _) = BrokerClient::acquire(
        &codex_home,
        identity.clone(),
        launch.clone(),
        initialize_params(),
        Some(Duration::from_secs(5)),
        cancel_elicitation_sender(),
    )
    .await?;

    first
        .list_tools_with_connector_ids(/*params*/ None, Some(Duration::from_secs(5)))
        .await?;
    second
        .list_resources(/*params*/ None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(startup_log_entries(&startup_log)?.len(), 1);

    first.release().await?;
    second
        .list_resource_templates(/*params*/ None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(startup_log_entries(&startup_log)?.len(), 1);

    second.release().await?;
    time::sleep(SERVER_IDLE_GRACE + Duration::from_millis(500)).await;

    let (mut third, _) = BrokerClient::acquire(
        &codex_home,
        identity,
        launch,
        initialize_params(),
        Some(Duration::from_secs(5)),
        cancel_elicitation_sender(),
    )
    .await?;
    third
        .list_tools_with_connector_ids(/*params*/ None, Some(Duration::from_secs(5)))
        .await?;
    assert_eq!(startup_log_entries(&startup_log)?.len(), 2);
    third.release().await?;

    handle.abort();
    Ok(())
}

#[tokio::test]
#[cfg(unix)]
async fn broker_routes_elicitation_to_active_caller() -> Result<()> {
    let temp = tempfile::TempDir::new()?;
    let codex_home = temp.path().join("codex-home");
    tokio::fs::create_dir(&codex_home).await?;
    let socket_path = control_socket_path(&codex_home);
    let handle = tokio::spawn(run_broker(socket_path.clone()));
    let stream = wait_for_test_socket(&socket_path).await?;
    drop(stream);

    let startup_log = temp.path().join("startups.log");
    let elicitation_log = temp.path().join("elicitations.log");
    let server = write_stdio_test_server(temp.path())?;
    let mut transport = stdio_test_server_transport(server, &startup_log);
    let McpServerTransportConfig::Stdio { env, .. } = &mut transport else {
        unreachable!("test helper returns stdio transport");
    };
    env.as_mut().expect("test helper sets env").insert(
        "MCP_TEST_ELICITATION_LOG".to_string(),
        elicitation_log.to_string_lossy().to_string(),
    );
    let config = server_config(transport, McpServerProcessReuseScope::Cwd);
    let (identity, launch) = reusable_stdio_identity(&config, &runtime(temp.path()))?
        .expect("local stdio transport should be reusable");

    let first_count = Arc::new(AtomicUsize::new(0));
    let second_count = Arc::new(AtomicUsize::new(0));
    let (mut first, _) = BrokerClient::acquire(
        &codex_home,
        identity.clone(),
        launch.clone(),
        initialize_params(),
        Some(Duration::from_secs(5)),
        counting_elicitation_sender(Arc::clone(&first_count)),
    )
    .await?;
    let (mut second, _) = BrokerClient::acquire(
        &codex_home,
        identity,
        launch,
        initialize_params(),
        Some(Duration::from_secs(5)),
        counting_elicitation_sender(Arc::clone(&second_count)),
    )
    .await?;

    second
        .call_tool(
            "ask".to_string(),
            Some(json!({})),
            /*meta*/ None,
            Some(Duration::from_secs(5)),
        )
        .await?;
    assert_eq!(first_count.load(Ordering::SeqCst), 0);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);

    first
        .call_tool(
            "ask".to_string(),
            Some(json!({})),
            /*meta*/ None,
            Some(Duration::from_secs(5)),
        )
        .await?;
    assert_eq!(first_count.load(Ordering::SeqCst), 1);
    assert_eq!(second_count.load(Ordering::SeqCst), 1);
    assert_eq!(startup_log_entries(&elicitation_log)?.len(), 2);

    first.release().await?;
    second.release().await?;
    handle.abort();
    Ok(())
}

async fn wait_for_test_socket(socket_path: &Path) -> Result<UnixStream> {
    timeout(Duration::from_secs(2), async {
        loop {
            match UnixStream::connect(socket_path).await {
                Ok(stream) => return Ok(stream),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
                    time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for broker test socket"))?
}
