use super::*;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCRequest;
use codex_app_server_protocol::RequestId;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

enum Reply {
    Page {
        request_id: RequestId,
        next_cursor: Option<String>,
    },
    Error(RequestId),
    Disconnect,
}

struct GateServer {
    client: Option<RemoteAppServerClient>,
    url: String,
    requests: mpsc::UnboundedReceiver<JSONRPCRequest>,
    replies: mpsc::UnboundedSender<Reply>,
    task: tokio::task::JoinHandle<()>,
}

async fn connect(url: &str) -> RemoteAppServerClient {
    RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: url.to_string(),
            auth_token: None,
        },
        client_name: "feature-gate-test".into(),
        client_version: "0.0.0".into(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 8,
    })
    .await
    .unwrap()
}

impl GateServer {
    async fn start(connections: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (requests_tx, requests) = mpsc::unbounded_channel();
        let (replies, mut replies_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for _ in 0..connections {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                loop {
                    let reply = tokio::select! {
                        Some(reply) = replies_rx.recv() => match reply {
                            Reply::Page { request_id, next_cursor } => json!({"id": request_id, "result": {"data": [], "nextCursor": next_cursor}}),
                            Reply::Error(id) => json!({"id": id, "error": {"code": -32600, "message": "rejected"}}),
                            Reply::Disconnect => break,
                        },
                        message = socket.next() => {
                            let Some(Ok(Message::Text(text))) = message else { break };
                            let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap() else { continue };
                            if request.method == "initialize" {
                                json!({"id": request.id, "result": {"userAgent": "feature-gate-test"}})
                            } else {
                                assert_eq!(request.method, "experimentalFeature/list");
                                requests_tx.send(request).unwrap();
                                continue;
                            }
                        }
                    };
                    socket
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                }
            }
        });
        let client = Some(connect(&url).await);
        Self {
            client,
            url,
            requests,
            replies,
            task,
        }
    }

    fn handle(&self) -> AppServerRequestHandle {
        AppServerRequestHandle::Remote(self.client.as_ref().unwrap().request_handle())
    }

    async fn next(&mut self) -> JSONRPCRequest {
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn shutdown(&mut self) {
        if let Some(client) = self.client.take() {
            client.shutdown().await.unwrap();
        }
    }

    async fn finish(mut self) {
        self.shutdown().await;
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn background_feature_jobs_release_slots_and_weak_gates_after_transport_completion() {
    enum Completion {
        Shutdown,
        Disconnect,
        LateReply,
        Error,
    }
    for completion in [
        Completion::Shutdown,
        Completion::Disconnect,
        Completion::LateReply,
        Completion::Error,
    ] {
        let mut server = GateServer::start(/*connections*/ 2).await;
        let gate = connection_gate(server.handle());
        let weak = Arc::downgrade(&gate);
        let slots = Arc::clone(&gate.slots);
        let caller = tokio::spawn(fetch(server.handle(), ThreadId::new()));
        let request = server.next().await;
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(gate);
        assert!(weak.upgrade().is_some());
        match completion {
            Completion::Shutdown => server.shutdown().await,
            Completion::Disconnect => server.replies.send(Reply::Disconnect).unwrap(),
            Completion::LateReply => server
                .replies
                .send(Reply::Page {
                    request_id: request.id,
                    next_cursor: Some("ignored-next".into()),
                })
                .unwrap(),
            Completion::Error => server.replies.send(Reply::Error(request.id)).unwrap(),
        }
        let restored = tokio::time::timeout(
            Duration::from_secs(/*secs*/ 10),
            slots.acquire_many_owned(MAX_OUTSTANDING as u32),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
            while weak.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(restored);
        server.shutdown().await;
        server.client = Some(connect(&server.url).await);
        let reconnected = connection_gate(server.handle());
        assert!(
            !GATES
                .lock()
                .unwrap()
                .iter()
                .any(|entry| Weak::ptr_eq(entry, &weak))
        );
        let caller = tokio::spawn(fetch(server.handle(), ThreadId::new()));
        let request = server.next().await;
        assert_eq!(
            request.params.as_ref().unwrap()["cursor"],
            serde_json::Value::Null
        );
        server
            .replies
            .send(Reply::Page {
                request_id: request.id,
                next_cursor: None,
            })
            .unwrap();
        let result = caller.await.unwrap();
        drop(reconnected);
        server.finish().await;
        assert_eq!(result, Ok(Vec::new()));
    }
}

#[tokio::test]
async fn background_feature_admission_cancellation_and_pagination_share_one_deadline() {
    let mut server = GateServer::start(/*connections*/ 1).await;
    let gate = connection_gate(server.handle());
    let held = Arc::clone(&gate.slots)
        .acquire_many_owned(MAX_OUTSTANDING as u32)
        .await
        .unwrap();
    let mut cancelled = Box::pin(fetch(server.handle(), ThreadId::new()));
    assert!(futures::poll!(&mut cancelled).is_pending());
    drop(cancelled);
    {
        let _registry = GATES.lock().unwrap();
        assert_eq!(Arc::strong_count(&gate), 1);
    }
    let mut waiting = Box::pin(fetch(server.handle(), ThreadId::new()));
    tokio::time::pause();
    let deadline = Instant::now() + Duration::from_secs(/*secs*/ 5);
    assert!(futures::poll!(&mut waiting).is_pending());
    tokio::time::advance(Duration::from_secs(/*secs*/ 3)).await;
    drop(held);
    let mut caller = tokio::spawn(waiting);
    tokio::time::resume();
    let first = server.next().await;
    server
        .replies
        .send(Reply::Page {
            request_id: first.id.clone(),
            next_cursor: Some("page-two".into()),
        })
        .unwrap();
    let second = server.next().await;
    assert_eq!(second.id, first.id);
    assert_eq!(second.params.as_ref().unwrap()["cursor"], "page-two");
    tokio::time::pause();
    tokio::time::advance(deadline.saturating_duration_since(Instant::now())).await;
    let result = match tokio::time::timeout(Duration::from_secs(/*secs*/ 10), &mut caller).await {
        Ok(result) => result.unwrap(),
        Err(error) => {
            caller.abort();
            let _ = caller.await;
            tokio::time::resume();
            server.finish().await;
            panic!("background feature discovery did not finish: {error}");
        }
    };
    let completed_at = Instant::now();
    tokio::time::resume();
    assert_eq!(
        result,
        Err("Experimental feature discovery timed out".into())
    );
    // Tokio's millisecond timer wheel rounds deadlines up to the end of a tick.
    assert!(completed_at >= deadline);
    assert!(completed_at <= deadline + Duration::from_millis(/*millis*/ 1));
    assert_eq!(gate.slots.available_permits(), MAX_OUTSTANDING - 1);
    server
        .replies
        .send(Reply::Page {
            request_id: second.id,
            next_cursor: Some("never-requested".into()),
        })
        .unwrap();
    let restored = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        Arc::clone(&gate.slots).acquire_many_owned(MAX_OUTSTANDING as u32),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(server.requests.try_recv().is_err());
    drop(restored);
    drop(gate);
    server.finish().await;
}
