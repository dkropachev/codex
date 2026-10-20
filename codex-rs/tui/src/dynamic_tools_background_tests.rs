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
