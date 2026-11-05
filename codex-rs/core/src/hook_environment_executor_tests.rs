//! Exercise hook launch deadlines across a real stalled exec-server transport.

use super::*;
use crate::environment_selection::TurnEnvironmentState;
use crate::session::tests::make_session_and_context;
use codex_exec_server::Environment;
use codex_network_proxy::EnvironmentNetworkPolicy;
use codex_network_proxy::NetworkProxyConfig;
use codex_protocol::protocol::EnvironmentConfigState;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::task::AbortOnDropHandle;

#[tokio::test]
async fn launch_metadata_wait_consumes_the_hook_timeout() -> anyhow::Result<()> {
    let (session, turn) = make_session_and_context().await;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let remote = Arc::new(Environment::create_for_tests(Some(format!(
        "ws://{}",
        listener.local_addr()?
    )))?);
    let (metadata, received) = oneshot::channel();
    let (finish, finished) = oneshot::channel();
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut socket = accept_async(listener.accept().await.unwrap().0)
            .await
            .unwrap();
        let initialize = socket.next().await.unwrap().unwrap();
        let initialize: Value = serde_json::from_str(initialize.to_text().unwrap()).unwrap();
        assert_eq!(initialize["method"], "initialize");
        socket
            .send(Message::Text(
                json!({"id": initialize["id"], "result": {"sessionId": "hook-timeout"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let initialized = socket.next().await.unwrap().unwrap();
        let initialized: Value = serde_json::from_str(initialized.to_text().unwrap()).unwrap();
        assert_eq!(initialized["method"], "initialized");
        let request = socket.next().await.unwrap().unwrap();
        let request: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
        assert_eq!(request["method"], "environment/info");
        metadata.send(()).unwrap();
        // Hold metadata longer than the hook budget without starting any process.
        finished.await.unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(/*secs*/ 3), remote.wait_until_ready()).await??;
    let mut environment = turn
        .initial_environments
        .primary()
        .expect("ready local fixture")
        .clone();
    let mut config = environment.config().clone();
    config.network_policy = Some(EnvironmentNetworkPolicy::from_config(
        &NetworkProxyConfig::default(),
        /*managed_allowed_domains_only*/ false,
    ));
    environment.selection.config = EnvironmentConfigState::Ready(config);
    environment.environment = remote;
    let environment_id = environment.selection.environment_id.clone();
    let executor = EnvironmentCommandExecutor {
        environments: TurnEnvironmentSnapshot {
            environments: vec![TurnEnvironmentState::Ready(environment)],
        },
        session: Arc::new(session),
        turn: Arc::new(turn),
    };
    let result = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 3),
        executor.execute(
            &environment_id,
            "echo hook",
            &HashMap::new(),
            "{}",
            Duration::from_millis(/*millis*/ 500),
        ),
    )
    .await
    .expect("metadata must not outlive the hook budget");
    let Err(error) = result else {
        panic!("stalled metadata should time out");
    };
    tokio::time::timeout(Duration::from_secs(/*secs*/ 1), received).await??;
    assert_eq!(
        error.to_string(),
        "environment command hook timed out during launch"
    );
    finish.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(/*secs*/ 1), server).await??;
    Ok(())
}
