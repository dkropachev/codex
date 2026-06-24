use super::AuthRequestTelemetryContext;
use super::ModelClient;
use super::PendingUnauthorizedRetry;
use super::UnauthorizedRecoveryExecution;
use super::X_CODEX_INSTALLATION_ID_HEADER;
use super::X_CODEX_PARENT_THREAD_ID_HEADER;
use super::X_CODEX_TURN_METADATA_HEADER;
use super::X_CODEX_WINDOW_ID_HEADER;
use super::X_OPENAI_SUBAGENT_HEADER;
use crate::AttestationContext;
use crate::AttestationProvider;
use crate::GenerateAttestationFuture;
use base64::Engine as _;
use codex_api::ApiError;
use codex_api::RealtimeEventParser;
use codex_api::RealtimeOutputModality;
use codex_api::RealtimeSessionConfig;
use codex_api::RealtimeSessionMode;
use codex_api::ResponseEvent;
use codex_app_server_protocol::AuthMode;
use codex_config::config_toml::AccountPoolDefinitionToml;
use codex_config::config_toml::AccountPoolPolicyToml;
use codex_config::config_toml::AccountPoolToml;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountPoolOperationKind;
use codex_login::AccountPoolUsageBucket;
use codex_login::AuthManager;
use codex_login::AuthManagerConfig;
use codex_login::CodexAuth;
use codex_model_provider::BearerAuthProvider;
use codex_model_provider_info::CHATGPT_CODEX_BASE_URL;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_otel::SessionTelemetry;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_rollout_trace::ExecutionStatus;
use codex_rollout_trace::InferenceTraceAttempt;
use codex_rollout_trace::InferenceTraceContext;
use codex_rollout_trace::RawTraceEventPayload;
use codex_rollout_trace::RolloutTrace;
use codex_rollout_trace::TraceWriter;
use codex_rollout_trace::replay_bundle;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

fn test_model_client(session_source: SessionSource) -> ModelClient {
    test_model_client_with_parent(session_source, /*parent_thread_id*/ None)
}

fn test_model_client_with_parent(
    session_source: SessionSource,
    parent_thread_id: Option<ThreadId>,
) -> ModelClient {
    let provider = create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses);
    let thread_id = ThreadId::new();
    ModelClient::new(
        /*auth_manager*/ None,
        thread_id.into(),
        thread_id,
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        provider,
        session_source,
        parent_thread_id,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*attestation_provider*/ None,
    )
}

fn test_model_info() -> ModelInfo {
    serde_json::from_value(json!({
        "slug": "gpt-test",
        "display_name": "gpt-test",
        "description": "desc",
        "default_reasoning_level": "medium",
        "supported_reasoning_levels": [
            {"effort": "medium", "description": "medium"}
        ],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "upgrade": null,
        "base_instructions": "base instructions",
        "model_messages": null,
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "default_verbosity": null,
        "apply_patch_tool_type": null,
        "truncation_policy": {"mode": "bytes", "limit": 10000},
        "supports_parallel_tool_calls": false,
        "supports_image_detail_original": false,
        "context_window": 272000,
        "auto_compact_token_limit": null,
        "experimental_supported_tools": []
    }))
    .expect("deserialize test model info")
}

fn test_session_telemetry() -> SessionTelemetry {
    SessionTelemetry::new(
        ThreadId::new(),
        "gpt-test",
        "gpt-test",
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test-originator".to_string(),
        /*log_user_prompts*/ false,
        "test-terminal".to_string(),
        SessionSource::Cli,
    )
}

struct AccountPoolClientTestConfig {
    codex_home: PathBuf,
    chatgpt_base_url: String,
    account_pool: AccountPoolToml,
}

impl AuthManagerConfig for AccountPoolClientTestConfig {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        AuthCredentialsStoreMode::File
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        None
    }

    fn chatgpt_base_url(&self) -> String {
        self.chatgpt_base_url.clone()
    }

    fn account_pool(&self) -> Option<AccountPoolToml> {
        Some(self.account_pool.clone())
    }
}

#[derive(Default)]
struct TagCollectorVisitor {
    tags: BTreeMap<String, String>,
}

impl Visit for TagCollectorVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.tags
            .insert(field.name().to_string(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.tags
            .insert(field.name().to_string(), format!("{value:?}"));
    }
}

#[derive(Clone)]
struct TagCollectorLayer {
    tags: Arc<Mutex<BTreeMap<String, String>>>,
}

impl<S> Layer<S> for TagCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
        if event.metadata().target() != "feedback_tags" {
            return;
        }
        let mut visitor = TagCollectorVisitor::default();
        event.record(&mut visitor);
        self.tags.lock().unwrap().extend(visitor.tags);
    }
}

fn started_inference_attempt(temp: &TempDir) -> anyhow::Result<InferenceTraceAttempt> {
    let writer = Arc::new(TraceWriter::create(
        temp.path(),
        "trace-1".to_string(),
        "rollout-1".to_string(),
        "thread-root".to_string(),
    )?);
    writer.append(RawTraceEventPayload::ThreadStarted {
        thread_id: "thread-root".to_string(),
        agent_path: "/root".to_string(),
        metadata_payload: None,
    })?;
    writer.append(RawTraceEventPayload::CodexTurnStarted {
        codex_turn_id: "turn-1".to_string(),
        thread_id: "thread-root".to_string(),
    })?;

    let inference_trace = InferenceTraceContext::enabled(
        writer,
        "thread-root".to_string(),
        "turn-1".to_string(),
        "gpt-test".to_string(),
        "test-provider".to_string(),
    );
    let attempt = inference_trace.start_attempt();
    attempt.record_started(&json!({
        "model": "gpt-test",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "hello"}]
        }],
    }));
    Ok(attempt)
}

fn output_message(id: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: Some(id.to_string()),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn input_message(text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
    }
}

fn prompt_with_input(input: Vec<ResponseItem>) -> crate::client_common::Prompt {
    crate::client_common::Prompt {
        input,
        ..Default::default()
    }
}

fn write_account_pool_member_auth(
    codex_home: &Path,
    account_id: &str,
    email: &str,
) -> anyhow::Result<()> {
    let account_home = codex_home.join("accounts").join(account_id);
    fs::create_dir_all(&account_home)?;
    let encode = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&value).expect("serialize jwt part"))
    };
    let jwt = format!(
        "{}.{}.sig",
        encode(json!({"alg": "none"})),
        encode(json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_account_id": account_id,
                "user_id": format!("user-{account_id}")
            }
        }))
    );
    let auth = json!({
        "auth_mode": AuthMode::Chatgpt,
        "tokens": {
            "id_token": jwt,
            "access_token": format!("access-{account_id}"),
            "refresh_token": format!("refresh-{account_id}"),
            "account_id": account_id,
        },
        "last_refresh": chrono::Utc::now(),
    });
    fs::write(
        account_home.join("auth.json"),
        serde_json::to_string_pretty(&auth)?,
    )?;
    Ok(())
}

fn load_balance_account_pool_toml() -> AccountPoolToml {
    AccountPoolToml {
        enabled: true,
        default_pool: Some("codex-pro".to_string()),
        pools: [(
            "codex-pro".to_string(),
            AccountPoolDefinitionToml {
                provider: "openai".to_string(),
                policy: AccountPoolPolicyToml::LoadBalance,
                accounts: vec!["work-pro".to_string(), "personal-pro".to_string()],
            },
        )]
        .into(),
    }
}

async fn mount_account_pool_usage(server: &MockServer, account_id: &str, used_percent: u64) {
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .and(header(
            "authorization",
            format!("Bearer access-{account_id}"),
        ))
        .and(header("chatgpt-account-id", account_id))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": { "used_percent": used_percent }
            }
        })))
        .expect(1)
        .mount(server)
        .await;
}

async fn account_pool_model_client(
    codex_home: &Path,
    chatgpt_base_url: String,
    provider: ModelProviderInfo,
) -> ModelClient {
    let config = AccountPoolClientTestConfig {
        codex_home: codex_home.to_path_buf(),
        chatgpt_base_url,
        account_pool: load_balance_account_pool_toml(),
    };
    let auth_manager =
        AuthManager::shared_from_config(&config, /*enable_codex_api_key_env*/ false).await;
    ModelClient::new(
        Some(auth_manager),
        SessionId::new(),
        ThreadId::new(),
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        provider,
        SessionSource::Cli,
        /*parent_thread_id*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        /*attestation_provider*/ None,
    )
}

async fn replay_until_cancelled(temp: &TempDir) -> anyhow::Result<RolloutTrace> {
    let mut rollout = replay_bundle(temp.path())?;
    for _ in 0..50 {
        let inference = rollout
            .inference_calls
            .values()
            .next()
            .expect("inference should be reduced");
        if inference.execution.status == ExecutionStatus::Cancelled {
            return Ok(rollout);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        rollout = replay_bundle(temp.path())?;
    }
    Ok(rollout)
}

struct NotifyAfterEventStream {
    events: VecDeque<ResponseEvent>,
    yielded: usize,
    notify_after: usize,
    notify: Arc<Notify>,
}

impl futures::Stream for NotifyAfterEventStream {
    type Item = std::result::Result<ResponseEvent, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(event) = self.events.pop_front() else {
            return Poll::Pending;
        };
        self.yielded += 1;
        if self.yielded == self.notify_after {
            self.notify.notify_one();
        }
        Poll::Ready(Some(Ok(event)))
    }
}

#[test]
fn build_subagent_headers_sets_other_subagent_label() {
    let client = test_model_client(SessionSource::SubAgent(SubAgentSource::Other(
        "memory_consolidation".to_string(),
    )));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[test]
fn build_subagent_headers_sets_internal_memory_consolidation_label() {
    let client = test_model_client(SessionSource::Internal(
        InternalSessionSource::MemoryConsolidation,
    ));
    let headers = client.build_subagent_headers();
    let value = headers
        .get(X_OPENAI_SUBAGENT_HEADER)
        .and_then(|value| value.to_str().ok());
    assert_eq!(value, Some("memory_consolidation"));
}

#[test]
fn build_ws_client_metadata_includes_window_lineage_and_turn_metadata() {
    let parent_thread_id = ThreadId::new();
    let client = test_model_client_with_parent(
        SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
            parent_thread_id,
            depth: 2,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
        }),
        Some(parent_thread_id),
    );

    client.advance_window_generation();

    let client_metadata = client.build_ws_client_metadata(
        Some(r#"{"turn_id":"turn-123"}"#),
        /*use_responses_lite*/ false,
    );
    let thread_id = client.state.thread_id;
    assert_eq!(
        client_metadata,
        std::collections::HashMap::from([
            (
                X_CODEX_INSTALLATION_ID_HEADER.to_string(),
                "11111111-1111-4111-8111-111111111111".to_string(),
            ),
            (
                X_CODEX_WINDOW_ID_HEADER.to_string(),
                format!("{thread_id}:1"),
            ),
            (
                X_OPENAI_SUBAGENT_HEADER.to_string(),
                "collab_spawn".to_string(),
            ),
            (
                X_CODEX_PARENT_THREAD_ID_HEADER.to_string(),
                parent_thread_id.to_string(),
            ),
            (
                X_CODEX_TURN_METADATA_HEADER.to_string(),
                r#"{"turn_id":"turn-123"}"#.to_string(),
            ),
        ])
    );
}

#[tokio::test]
async fn summarize_memories_returns_empty_for_empty_input() {
    let client = test_model_client(SessionSource::Cli);
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();

    let output = client
        .summarize_memories(
            Vec::new(),
            &model_info,
            /*effort*/ None,
            &session_telemetry,
        )
        .await
        .expect("empty summarize request should succeed");
    assert_eq!(output.len(), 0);
}

#[tokio::test]
async fn current_client_setup_honors_account_pool_pins_for_operation_kinds() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;

    write_account_pool_member_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_account_pool_member_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    mount_account_pool_usage(&server, "work-pro", /*used_percent*/ 90).await;
    mount_account_pool_usage(&server, "personal-pro", /*used_percent*/ 10).await;

    let provider = ModelProviderInfo::create_openai_provider(Some("https://example.com/v1".into()));
    let client = account_pool_model_client(codex_home.path(), server.uri(), provider).await;
    let model_info = test_model_info();
    let unpinned = client
        .current_client_setup(
            Some(&model_info.slug),
            AccountPoolOperationKind::Stream,
            /*pinned_selection*/ None,
        )
        .await?;
    let mut pinned_selection = unpinned
        .account_pool_selection
        .clone()
        .expect("load-balanced setup should select account-pool auth");
    assert_eq!(pinned_selection.account_id, "personal-pro");
    pinned_selection.account_id = "work-pro".to_string();

    for operation_kind in [
        AccountPoolOperationKind::Stream,
        AccountPoolOperationKind::RealtimeSetup,
        AccountPoolOperationKind::Prewarm,
        AccountPoolOperationKind::Compaction,
        AccountPoolOperationKind::MemorySummarize,
    ] {
        let model = match operation_kind {
            AccountPoolOperationKind::RealtimeSetup => None,
            AccountPoolOperationKind::Stream
            | AccountPoolOperationKind::Prewarm
            | AccountPoolOperationKind::Compaction
            | AccountPoolOperationKind::MemorySummarize => Some(model_info.slug.as_str()),
        };
        let setup = client
            .current_client_setup(model, operation_kind, Some(&pinned_selection))
            .await?;
        let selected = setup
            .account_pool_selection
            .expect("pinned setup should select account-pool auth");
        assert_eq!(selected.account_id, "work-pro");
        assert_eq!(selected.bucket, AccountPoolUsageBucket::Regular);
        assert_eq!(
            setup
                .api_auth
                .to_auth_headers()
                .get("ChatGPT-Account-ID")
                .and_then(|header| header.to_str().ok()),
            Some("work-pro")
        );
    }

    server.verify().await;
    Ok(())
}

#[tokio::test]
async fn realtime_webrtc_sideband_uses_same_account_pool_auth() -> anyhow::Result<()> {
    let codex_home = TempDir::new()?;
    let server = MockServer::start().await;
    write_account_pool_member_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_account_pool_member_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    mount_account_pool_usage(&server, "work-pro", /*used_percent*/ 90).await;
    mount_account_pool_usage(&server, "personal-pro", /*used_percent*/ 10).await;

    Mock::given(method("POST"))
        .and(path("/backend-api/codex/realtime/calls"))
        .and(header("authorization", "Bearer access-personal-pro"))
        .and(header("chatgpt-account-id", "personal-pro"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Location", "/v1/realtime/calls/rtc_account_pool")
                .set_body_string("v=0\r\n"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = ModelProviderInfo::create_openai_provider(Some(format!(
        "{}/backend-api/codex",
        server.uri()
    )));
    let client = account_pool_model_client(codex_home.path(), server.uri(), provider).await;

    let call = client
        .create_realtime_call_with_headers(
            "v=offer\r\n".to_string(),
            RealtimeSessionConfig {
                instructions: "test instructions".to_string(),
                model: Some("gpt-realtime".to_string()),
                session_id: Some("session-1".to_string()),
                event_parser: RealtimeEventParser::RealtimeV2,
                session_mode: RealtimeSessionMode::Conversational,
                output_modality: RealtimeOutputModality::Audio,
                voice: RealtimeVoice::Marin,
            },
            http::HeaderMap::new(),
        )
        .await?;

    assert_eq!(call.call_id, "rtc_account_pool");
    assert_eq!(call.sdp, "v=0\r\n");
    assert_eq!(
        call.sideband_headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer access-personal-pro")
    );
    assert_eq!(
        call.sideband_headers
            .get("ChatGPT-Account-ID")
            .and_then(|value| value.to_str().ok()),
        Some("personal-pro")
    );

    server.verify().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_preconnect_with_account_pool_reuses_selected_account() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let codex_home = TempDir::new()?;
    let usage_server = MockServer::start().await;
    write_account_pool_member_auth(codex_home.path(), "work-pro", "work@example.com")?;
    write_account_pool_member_auth(codex_home.path(), "personal-pro", "personal@example.com")?;
    mount_account_pool_usage(&usage_server, "work-pro", /*used_percent*/ 90).await;
    mount_account_pool_usage(&usage_server, "personal-pro", /*used_percent*/ 10).await;

    let websocket_server = start_websocket_server(vec![vec![vec![
        ev_response_created("resp-1"),
        ev_completed("resp-1"),
    ]]])
    .await;
    let provider =
        ModelProviderInfo::create_openai_provider(Some(format!("{}/v1", websocket_server.uri())));
    let client = account_pool_model_client(codex_home.path(), usage_server.uri(), provider).await;
    let model_info = test_model_info();
    let session_telemetry = test_session_telemetry();
    let mut client_session = client.new_session();

    client_session
        .preconnect_websocket(&session_telemetry, &model_info)
        .await
        .expect("websocket preconnect failed");
    let prompt = prompt_with_input(vec![input_message("hello")]);
    let mut stream = client_session
        .stream(
            &prompt,
            &model_info,
            &session_telemetry,
            /*effort*/ None,
            ReasoningSummary::Auto,
            /*service_tier*/ None,
            /*turn_metadata_header*/ None,
            &InferenceTraceContext::disabled(),
        )
        .await
        .expect("websocket stream failed");

    while let Some(event) = stream.next().await {
        if matches!(event, Ok(ResponseEvent::Completed { .. })) {
            break;
        }
    }

    assert_eq!(
        websocket_server
            .single_handshake()
            .header("chatgpt-account-id")
            .as_deref(),
        Some("personal-pro")
    );
    assert_eq!(
        websocket_server
            .single_handshake()
            .header("authorization")
            .as_deref(),
        Some("Bearer access-personal-pro")
    );
    assert_eq!(websocket_server.single_connection().len(), 1);

    usage_server.verify().await;
    websocket_server.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn dropped_response_stream_traces_cancelled_partial_output() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;

    // The provider has produced one complete output item, but no terminal
    // response.completed event. The harness has enough information to keep this
    // item in history, so the trace should preserve it when the stream is
    // abandoned.
    let item = output_message("msg-1", "partial answer");
    let api_stream = futures::stream::iter([Ok(ResponseEvent::OutputItemDone(item))])
        .chain(futures::stream::pending());
    let (mut stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
    );

    let observed = stream
        .next()
        .await
        .expect("mapped stream should yield output item")?;
    assert!(matches!(observed, ResponseEvent::OutputItemDone(_)));

    // Dropping the consumer is how turn interruption/preemption stops polling
    // the provider stream. The mapper task observes that drop asynchronously
    // and records cancellation using the output items it has already seen.
    drop(stream);

    // Cancellation is recorded by the mapper task after Drop wakes it, so the
    // replay may need a short wait before the terminal event appears on disk.
    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[tokio::test]
async fn response_stream_records_last_model_feedback_ids() {
    let tags = Arc::new(Mutex::new(BTreeMap::new()));
    let _guard = tracing_subscriber::registry()
        .with(TagCollectorLayer { tags: tags.clone() })
        .set_default();

    let api_stream = futures::stream::iter([
        Ok(ResponseEvent::Created),
        Ok(ResponseEvent::Completed {
            response_id: "resp-123".to_string(),
            token_usage: None,
            end_turn: Some(true),
        }),
    ]);
    let (mut stream, _) = super::map_response_events(
        Some("req-123".to_string()),
        api_stream,
        test_session_telemetry(),
        InferenceTraceAttempt::disabled(),
    );

    while stream.next().await.is_some() {}

    let tags = tags.lock().unwrap().clone();
    assert_eq!(
        tags.get("last_model_request_id").map(String::as_str),
        Some("\"req-123\"")
    );
    assert_eq!(
        tags.get("last_model_response_id").map(String::as_str),
        Some("\"resp-123\"")
    );
}

#[tokio::test]
async fn dropped_backpressured_response_stream_traces_cancelled_partial_output()
-> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let attempt = started_inference_attempt(&temp)?;
    let backpressured_item_yielded = Arc::new(Notify::new());
    let mut events = VecDeque::new();
    for _ in 0..super::RESPONSE_STREAM_CHANNEL_CAPACITY {
        events.push_back(ResponseEvent::Created);
    }
    events.push_back(ResponseEvent::OutputItemDone(output_message(
        "msg-1",
        "partial answer",
    )));
    let api_stream = NotifyAfterEventStream {
        events,
        yielded: 0,
        notify_after: super::RESPONSE_STREAM_CHANNEL_CAPACITY + 1,
        notify: Arc::clone(&backpressured_item_yielded),
    };

    let (stream, _) = super::map_response_events(
        /*upstream_request_id*/ None,
        api_stream,
        test_session_telemetry(),
        attempt,
    );

    // Fill the mapper channel with non-terminal events, then yield one output
    // item. The mapper has observed that item and is blocked trying to send it
    // downstream, so dropping the consumer covers the send-failure path rather
    // than the `consumer_dropped` select branch.
    backpressured_item_yielded.notified().await;
    drop(stream);

    let rollout = replay_until_cancelled(&temp).await?;
    let inference = rollout
        .inference_calls
        .values()
        .next()
        .expect("inference should be reduced");

    assert_eq!(inference.execution.status, ExecutionStatus::Cancelled);
    assert_eq!(inference.response_item_ids.len(), 1);
    assert_eq!(rollout.raw_payloads.len(), 2);

    Ok(())
}

#[test]
fn auth_request_telemetry_context_tracks_attached_auth_and_retry_phase() {
    let auth_context = AuthRequestTelemetryContext::new(
        Some(AuthMode::Chatgpt),
        &BearerAuthProvider::for_test(Some("access-token"), Some("workspace-123")),
        PendingUnauthorizedRetry::from_recovery(UnauthorizedRecoveryExecution {
            mode: "managed",
            phase: "refresh_token",
        }),
    );

    assert_eq!(auth_context.auth_mode, Some("Chatgpt"));
    assert!(auth_context.auth_header_attached);
    assert_eq!(auth_context.auth_header_name, Some("authorization"));
    assert!(auth_context.retry_after_unauthorized);
    assert_eq!(auth_context.recovery_mode, Some("managed"));
    assert_eq!(auth_context.recovery_phase, Some("refresh_token"));
}

fn model_client_with_counting_attestation(
    include_attestation: bool,
) -> (ModelClient, Arc<AtomicUsize>) {
    #[derive(Debug)]
    struct CountingAttestationProvider {
        calls: Arc<AtomicUsize>,
    }

    impl AttestationProvider for CountingAttestationProvider {
        fn header_for_request(
            &self,
            _context: AttestationContext,
        ) -> GenerateAttestationFuture<'_> {
            let calls = self.calls.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::Relaxed) + 1;
                Some(http::HeaderValue::from_bytes(format!("v1.header-{call}").as_bytes()).unwrap())
            })
        }
    }

    let attestation_calls = Arc::new(AtomicUsize::new(0));
    let (auth_manager, provider) = if include_attestation {
        (
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
            ModelProviderInfo::create_openai_provider(Some(CHATGPT_CODEX_BASE_URL.to_string())),
        )
    } else {
        (
            None,
            create_oss_provider_with_base_url("https://example.com/v1", WireApi::Responses),
        )
    };
    let model_client = ModelClient::new(
        auth_manager,
        SessionId::new(),
        ThreadId::new(),
        /*installation_id*/ "11111111-1111-4111-8111-111111111111".to_string(),
        provider,
        SessionSource::Exec,
        /*parent_thread_id*/ None,
        /*enable_request_compression*/ false,
        /*include_timing_metrics*/ false,
        /*beta_features_header*/ None,
        Some(Arc::new(CountingAttestationProvider {
            calls: attestation_calls.clone(),
        })),
    );
    (model_client, attestation_calls)
}

#[tokio::test]
async fn websocket_handshake_includes_attestation_for_chatgpt_codex_responses() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ true);

    let headers = model_client
        .build_websocket_headers(/*turn_state*/ None, /*turn_metadata_header*/ None)
        .await;

    assert_eq!(
        headers
            .get(crate::attestation::X_OAI_ATTESTATION_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some("v1.header-1"),
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn non_chatgpt_codex_endpoints_omit_attestation_generation() {
    let (model_client, attestation_calls) =
        model_client_with_counting_attestation(/*include_attestation*/ false);
    let mut response_headers = http::HeaderMap::new();

    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        response_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut compaction_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        compaction_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }
    let mut realtime_headers = http::HeaderMap::new();
    if let Some(header_value) = model_client.generate_attestation_header_for().await {
        realtime_headers.insert(crate::attestation::X_OAI_ATTESTATION_HEADER, header_value);
    }

    assert_eq!(
        response_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        compaction_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(
        realtime_headers.get(crate::attestation::X_OAI_ATTESTATION_HEADER),
        None,
    );
    assert_eq!(attestation_calls.load(Ordering::Relaxed), 0);
}
