//! Prediction forks must suppress speculative requests while normal parents keep them.

use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use test_case::test_case;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test_case(ThreadHistoryMode::Legacy; "legacy")]
#[test_case(ThreadHistoryMode::Paginated; "paginated")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prediction_forks_do_not_launch_background_compaction(
    history_mode: ThreadHistoryMode,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-5.2")
        .with_provider_name("OpenAI")
        .with_provider_config("requires_openai_auth = true\nsupports_websockets = false")
        .with_root_config(
            "model_context_window = 100000\nmodel_auto_compact_token_limit = 90000\nmodel_post_turn_compact_threshold_percent = 0\nasync_compaction_threshold_percent = 65",
        )
        .enable_feature(Feature::AsyncCompaction)
        .disable_feature(Feature::TokenBudget)
        .disable_feature(Feature::EnableRequestCompression)
        .write(codex_home.path())?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("access-chatgpt").plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized()
        .await?;
    let parent = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(history_mode),
            ..Default::default()
        })
        .await?;
    let parent_id = parent.thread.id.clone();
    let parent_compaction = Arc::new(Notify::new());
    let received_parent_compaction = Arc::clone(&parent_compaction);
    let sampling_requests = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(move |request: &wiremock::Request| {
            let body: Value = request.body_json().expect("Responses request");
            let compacting = body["input"]
                .as_array()
                .expect("input items")
                .iter()
                .any(|item| item["type"] == "compaction_trigger");
            let events = if compacting {
                if request
                    .headers
                    .get("thread-id")
                    .is_some_and(|id| id == &parent_id)
                {
                    received_parent_compaction.notify_one();
                }
                vec![
                    json!({"type": "response.output_item.done", "item": {
                        "type": "compaction", "encrypted_content": "background-summary"
                    }}),
                    responses::ev_completed_with_tokens("background", /*total_tokens*/ 200),
                ]
            } else {
                let tokens = if sampling_requests.fetch_add(1, Ordering::SeqCst) == 0 {
                    200
                } else {
                    70_000
                };
                vec![
                    responses::ev_assistant_message("answer", "Original context preserved."),
                    responses::ev_completed_with_tokens("answer", tokens),
                ]
            };
            responses::sse_response(responses::sse(events))
                .insert_header("x-codex-turn-state", "sampling-state")
        })
        .mount(&server)
        .await;

    send_turn_and_wait(
        &mut mcp,
        &parent.thread.id,
        "Seed below the async threshold",
    )
    .await?;
    let fork_id = mcp
        .send_thread_fork_request(ThreadForkParams {
            thread_id: parent.thread.id.clone(),
            experimental_prediction_mode: true,
            ephemeral: true,
            exclude_turns: true,
            ..Default::default()
        })
        .await?;
    let child: ThreadForkResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(fork_id)).await??;
    for thread_id in [&child.thread.id, &child.thread.id, &parent.thread.id] {
        let completed = mcp
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Continue above the async threshold".into(),
                    text_elements: vec![],
                }],
                ..Default::default()
            })
            .await?;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
    }
    // This positive control proves inherited async configuration is active on the V2 route.
    timeout(DEFAULT_READ_TIMEOUT, parent_compaction.notified()).await?;
    // Drain session-owned tasks before inspecting the complete request history.
    let status = timeout(DEFAULT_READ_TIMEOUT, mcp.shutdown_gracefully()).await??;
    assert!(status.success());
    let requests = responses::received_responses_requests(&server).await;
    assert_eq!(
        requests
            .iter()
            .map(|request| (
                request.header("thread-id"),
                request.inputs_of_type("compaction_trigger").len(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (Some(parent.thread.id.clone()), 0),
            (Some(child.thread.id.clone()), 0),
            (Some(child.thread.id), 0),
            (Some(parent.thread.id.clone()), 0),
            (Some(parent.thread.id), 1),
        ],
    );
    Ok(())
}
