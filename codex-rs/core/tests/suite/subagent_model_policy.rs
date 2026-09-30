//! Verifies model-policy routing for v2 spawned agents at the responses boundary.

use anyhow::Result;
use codex_config::config_toml::ModelPolicyReasoningEffortToml;
use codex_config::config_toml::ModelPolicyRouteToml;
use codex_config::config_toml::ModelPolicyRuleToml;
use codex_config::config_toml::ModelPolicyToml;
use codex_features::Feature;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const ROOT_MODEL: &str = "gpt-5.4";
const ROUTED_MODEL: &str = "gpt-5.6-sol";
const ROOT_PROMPT: &str = "spawn a policy-routed worker";
const CHILD_PROMPT: &str = "inspect the policy-routed task";
const SPAWN_CALL_ID: &str = "spawn-policy-routed-worker";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    let body = match request
        .headers
        .get("content-encoding")
        .and_then(|encoding| encoding.to_str().ok())
    {
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            zstd::stream::decode_all(std::io::Cursor::new(&request.body)).ok()
        }
        _ => Some(request.body.clone()),
    };
    body.and_then(|body| String::from_utf8(body).ok())
        .is_some_and(|body| body.contains(text))
}

fn is_child_request(request: &wiremock::Request) -> bool {
    request
        .headers
        .get("x-openai-subagent")
        .and_then(|value| value.to_str().ok())
        == Some("collab_spawn")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_spawn_request_uses_model_policy_and_preserves_root_service_tier() -> Result<()> {
    let server = start_mock_server().await;

    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            !is_child_request(request)
                && body_contains(request, ROOT_PROMPT)
                && !body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("root-spawn"),
            ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "collaboration",
                "spawn_agent",
                &json!({
                    "message": CHILD_PROMPT,
                    "task_name": "policy_routed",
                    "fork_turns": "none",
                })
                .to_string(),
            ),
            ev_completed("root-spawn"),
        ]),
    )
    .await;
    let child_response = mount_sse_once_match(
        &server,
        is_child_request,
        sse(vec![
            ev_response_created("child-complete"),
            ev_assistant_message("child-complete-message", "done"),
            ev_completed("child-complete"),
        ]),
    )
    .await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            !is_child_request(request) && body_contains(request, SPAWN_CALL_ID)
        },
        sse(vec![
            ev_response_created("root-complete"),
            ev_assistant_message("root-complete-message", "worker started"),
            ev_completed("root-complete"),
        ]),
    )
    .await;

    let mut builder = test_codex().with_model(ROOT_MODEL).with_config(|config| {
        for feature in [Feature::Collab, Feature::MultiAgentV2] {
            config
                .features
                .enable(feature)
                .expect("test config should allow feature update");
        }
        config.service_tier = Some(ServiceTier::Fast.request_value().to_string());
        config.model_reasoning_effort = Some(ReasoningEffort::High);
        config.model_policy = Some(ModelPolicyToml {
            enabled: true,
            rules: vec![ModelPolicyRuleToml {
                source: Some(vec!["subagent.thread_spawn".to_string()]),
                route: ModelPolicyRouteToml {
                    model: Some(ROUTED_MODEL.to_string()),
                    service_tier: Some(ServiceTier::Flex),
                    reasoning_effort: Some(ModelPolicyReasoningEffortToml::Low),
                    ..Default::default()
                },
                ..Default::default()
            }],
            default_route: None,
        });
    });
    let test = builder.build_with_auto_env(&server).await?;
    let mut created_threads = test.thread_manager.subscribe_thread_created();

    test.submit_text_turn(ROOT_PROMPT).await?;
    let child_thread_id = created_threads.recv().await?;
    let child = test.thread_manager.get_thread(child_thread_id).await?;
    wait_for_event(child.as_ref(), |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let child_requests: Vec<_> = child_response
        .requests()
        .into_iter()
        .filter(|request| request.header("x-openai-subagent").as_deref() == Some("collab_spawn"))
        .collect();
    assert_eq!(child_requests.len(), 1);
    let request = &child_requests[0];
    let body = request.body_json();
    assert_eq!(
        (
            request.header("x-openai-subagent").as_deref(),
            body.get("model").and_then(Value::as_str),
            body.pointer("/reasoning/effort").and_then(Value::as_str),
            body.get("service_tier").and_then(Value::as_str),
        ),
        (
            Some("collab_spawn"),
            Some(ROUTED_MODEL),
            Some("low"),
            Some(ServiceTier::Fast.request_value()),
        )
    );

    Ok(())
}
