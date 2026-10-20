use super::*;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_protocol::JSONRPCMessage;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn background_api_key_timeouts_bound_unanswered_feature_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (models_tx, mut models_rx) = mpsc::channel(/*buffer*/ 1);
    let (feature_tx, mut feature_rx) = mpsc::channel(/*buffer*/ 12);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut counts = [0, 0];
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            let result = match request.method.as_str() {
                "initialize" => json!({"userAgent": "background-feature-test"}),
                "account/read" => {
                    json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": false, "workspaceRouting": null})
                }
                "model/list" => json!({"data": [], "nextCursor": null}),
                "experimentalFeature/list" => {
                    counts[0] += 1;
                    feature_tx.send(()).await.unwrap();
                    continue;
                }
                "turn/start" => {
                    counts[1] += 1;
                    assert_eq!(
                        request.params.as_ref().unwrap()["cyberAccessProgram"],
                        Value::Null
                    );
                    json!({"turn": {"id": "normal-turn", "rootTurnId": null, "items": [], "status": "inProgress", "error": null, "startedAt": null, "completedAt": null, "durationMs": null}})
                }
                method => panic!("unexpected RPC: {method}"),
            };
            socket
                .send(Message::Text(
                    json!({"id": request.id, "result": result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            if request.method == "model/list" {
                models_tx.send(()).await.unwrap();
            }
        }
        counts
    });
    let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
        endpoint: RemoteAppServerEndpoint::WebSocket {
            websocket_url: url,
            auth_token: None,
        },
        client_name: "background-feature-test".into(),
        client_version: "0.0.0".into(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: 8,
    })
    .await
    .unwrap();
    let mut features = Features::with_defaults();
    features.enable(Feature::CliDaybreak);
    let mut programs = Vec::new();
    let mut turns = Vec::new();
    for index in 0..12 {
        let features = features.clone();
        let handle = AppServerRequestHandle::Remote(client.request_handle());
        let thread_id = ThreadId::new().to_string();
        let target = thread_id.clone();
        let attempt = tokio::spawn(async move {
            background_turn_program(
                &features,
                &handle,
                &target,
                "test-model",
                "openai",
                /*enabled*/ false,
            )
            .await
        });
        // The first eight requests have reached the wire before their timers
        // advance. Once saturated, paused auto-advance also covers waiting for
        // admission without expecting a ninth feature request.
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), models_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if index < 8 {
            tokio::time::timeout(Duration::from_secs(/*secs*/ 10), feature_rx.recv())
                .await
                .unwrap()
                .unwrap();
        }
        tokio::time::pause();
        if index < 8 {
            tokio::time::advance(Duration::from_secs(/*secs*/ 5)).await;
        }
        let program = attempt.await.unwrap();
        tokio::time::resume();
        programs.push(program);
        // A successful turn response fences all preceding client writes.
        turns.push(
            start_turn(
                &AppServerRequestHandle::Remote(client.request_handle()),
                &thread_id,
                "send_message_to_thread",
                "continue".into(),
                /*model*/ None,
                /*sandbox_policy*/ None,
                /*cyber_access_program*/ None,
            )
            .await
            .map(|response| response.turn.id),
        );
    }
    let shutdown = client.shutdown().await;
    let counts = tokio::time::timeout(Duration::from_secs(/*secs*/ 10), server)
        .await
        .unwrap()
        .unwrap();
    shutdown.unwrap();
    assert_eq!(programs, vec![Ok(None); 12]);
    assert_eq!(turns, vec![Ok("normal-turn".to_string()); 12]);
    assert_eq!(counts, [8, 12]);
}

struct FeatureServer {
    client: RemoteAppServerClient,
    features: mpsc::Receiver<codex_app_server_protocol::JSONRPCRequest>,
    replies: mpsc::UnboundedSender<(RequestId, Value)>,
    task: tokio::task::JoinHandle<Vec<codex_app_server_protocol::JSONRPCRequest>>,
}

impl FeatureServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (features_tx, features) = mpsc::channel(/*buffer*/ 16);
        let (replies, mut replies_rx) = mpsc::unbounded_channel::<(RequestId, Value)>();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut requests = Vec::new();
            loop {
                let (id, result) = tokio::select! {
                    Some(reply) = replies_rx.recv() => reply,
                    message = socket.next() => {
                        let Some(Ok(Message::Text(text))) = message else { break };
                        let JSONRPCMessage::Request(request) = serde_json::from_str(&text).unwrap() else { continue };
                        let result = match request.method.as_str() {
                            "initialize" => json!({"userAgent": "background-feature-test"}),
                            "account/read" => json!({"account": {"type": "apiKey"}, "requiresOpenaiAuth": false, "workspaceRouting": null}),
                            "model/list" => json!({"data": [{
                                "id": "test-model", "model": "test-model", "displayName": "Test", "description": "Test",
                                "hidden": false, "supportedReasoningEfforts": [], "defaultReasoningEffort": "medium", "isDefault": true,
                                "availableAccessPrograms": {"cyber": ["standard", "daybreakBlue"]}
                            }], "nextCursor": null}),
                            "experimentalFeature/list" => {
                                features_tx.send(request.clone()).await.unwrap();
                                requests.push(request);
                                continue;
                            }
                            method => panic!("unexpected RPC: {method}"),
                        };
                        (request.id, result)
                    }
                };
                socket
                    .send(Message::Text(
                        json!({"id": id, "result": result}).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
            requests
        });
        let client = RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
            endpoint: RemoteAppServerEndpoint::WebSocket {
                websocket_url: url,
                auth_token: None,
            },
            client_name: "background-feature-test".into(),
            client_version: "0.0.0".into(),
            experimental_api: true,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        Self {
            client,
            features,
            replies,
            task,
        }
    }

    async fn next(&mut self) -> codex_app_server_protocol::JSONRPCRequest {
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), self.features.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn finish(self) -> Vec<codex_app_server_protocol::JSONRPCRequest> {
        self.client.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(/*secs*/ 10), self.task)
            .await
            .unwrap()
            .unwrap()
    }
}

fn program_attempt(
    client: &RemoteAppServerClient,
    thread_id: ThreadId,
    enabled: bool,
) -> tokio::task::JoinHandle<Result<Option<codex_app_server_protocol::CyberAccessProgram>, String>>
{
    let handle = AppServerRequestHandle::Remote(client.request_handle());
    tokio::spawn(async move {
        let mut features = Features::with_defaults();
        features.enable(Feature::CliDaybreak);
        background_turn_program(
            &features,
            &handle,
            &thread_id.to_string(),
            "test-model",
            "openai",
            enabled,
        )
        .await
    })
}

fn feature_page(enabled: bool, next_cursor: Option<&str>) -> Value {
    json!({"data": [{
        "name": Feature::ApiKeyCyberAccessPrograms.key(), "stage": "beta", "displayName": null,
        "description": null, "announcement": null, "enabled": enabled, "defaultEnabled": false
    }], "nextCursor": next_cursor})
}

#[tokio::test]
async fn background_feature_replies_preserve_thread_policy_and_connection_isolation() {
    let mut server = FeatureServer::start().await;
    let allowed = ThreadId::new();
    let blocked = ThreadId::new();
    let first = program_attempt(&server.client, allowed, /*enabled*/ true);
    let second = program_attempt(&server.client, blocked, /*enabled*/ true);
    let requests = [server.next().await, server.next().await];
    for request in requests.iter().rev() {
        let enabled = request.params.as_ref().unwrap()["threadId"] == allowed.to_string();
        server
            .replies
            .send((
                request.id.clone(),
                feature_page(enabled, /*next_cursor*/ None),
            ))
            .unwrap();
    }
    let results = [first.await.unwrap(), second.await.unwrap()];
    let mut pending = Vec::new();
    for _ in 0..8 {
        pending.push(program_attempt(
            &server.client,
            ThreadId::new(),
            /*enabled*/ false,
        ));
        server.next().await;
    }
    let mut independent = FeatureServer::start().await;
    let third = program_attempt(&independent.client, ThreadId::new(), /*enabled*/ true);
    let request = independent.next().await;
    independent
        .replies
        .send((
            request.id,
            feature_page(/*enabled*/ true, /*next_cursor*/ None),
        ))
        .unwrap();
    let independent_result = third.await.unwrap();
    let independent_requests = independent.finish().await;
    let all_requests = server.finish().await;
    let mut pending_results = Vec::new();
    for attempt in pending {
        pending_results.push(attempt.await.unwrap());
    }
    assert_eq!(
        results,
        [
            Ok(Some(
                codex_app_server_protocol::CyberAccessProgram::DaybreakBlue
            )),
            Ok(None)
        ]
    );
    assert_eq!(
        independent_result,
        Ok(Some(
            codex_app_server_protocol::CyberAccessProgram::DaybreakBlue
        ))
    );
    assert_eq!(pending_results, vec![Ok(None); 8]);
    assert_eq!([all_requests.len(), independent_requests.len()], [10, 1]);
}

#[tokio::test]
async fn background_feature_late_replies_do_not_resume_cancelled_callers() {
    let mut server = FeatureServer::start().await;
    let thread_id = ThreadId::new();
    let mut results = Vec::new();
    for cancel in [false, true] {
        let old = program_attempt(&server.client, thread_id, /*enabled*/ true);
        let request = server.next().await;
        if cancel {
            old.abort();
            assert!(old.await.unwrap_err().is_cancelled());
        } else {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(/*secs*/ 5)).await;
            results.push(old.await.unwrap());
            tokio::time::resume();
        }
        server
            .replies
            .send((
                request.id.clone(),
                feature_page(/*enabled*/ true, Some("old-next")),
            ))
            .unwrap();
        let retry = program_attempt(&server.client, thread_id, /*enabled*/ true);
        let fresh = server.next().await;
        assert_ne!(fresh.id, request.id);
        assert_eq!(fresh.params.as_ref().unwrap()["cursor"], Value::Null);
        server
            .replies
            .send((
                fresh.id,
                feature_page(/*enabled*/ false, /*next_cursor*/ None),
            ))
            .unwrap();
        results.push(retry.await.unwrap());
    }
    let requests = server.finish().await;
    assert_eq!(results, vec![Ok(None); 3]);
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .map(|request| &request.id)
            .collect::<HashSet<_>>()
            .len(),
        4
    );
}
