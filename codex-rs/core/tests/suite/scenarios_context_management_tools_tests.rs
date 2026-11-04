//! Outgoing context-tool definitions follow the captured acting model.

use super::*;
use codex_protocol::openai_models::ContextManagementToolMessages;
use codex_protocol::openai_models::ModelInfo;
use pretty_assertions::assert_eq;

fn configure_tools(model: &mut ModelInfo, description: &str) {
    let message = ToolMessage {
        description: Some(description.to_string()),
        parameters: Some(r#"{"type":"object","properties":{},"additionalProperties":false,"description":"Catalog arguments."}"#.to_string()),
    };
    model
        .model_messages
        .get_or_insert_default()
        .tools
        .get_or_insert_default()
        .context_management = Some(ContextManagementToolMessages {
        new_context: Some(message.clone()),
        get_context_remaining: Some(message),
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_context_tool_definitions_follow_captured_models() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_completed("first")]),
            sse(vec![ev_completed("second")]),
        ],
    )
    .await;
    let test = test_codex()
        .with_model_info_override("gpt-5.4", |model| {
            model.use_responses_lite = false;
            model.tool_mode = Some(ToolMode::Direct);
            configure_tools(model, "Catalog context guidance.");
        })
        .with_config(|config| {
            config
                .features
                .enable(Feature::TokenBudget)
                .expect("enable token budget");
            let catalog = config.model_catalog.as_mut().expect("model catalog");
            let mut updated = catalog
                .models
                .iter()
                .find(|model| model.slug == "gpt-5.4")
                .unwrap()
                .clone();
            updated.slug = "context-tools-updated".to_string();
            configure_tools(&mut updated, "");
            catalog.models.push(updated);
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_text_turn("Begin work.").await?;
    core_test_support::submit_thread_settings(
        &test.codex,
        codex_protocol::protocol::ThreadSettingsOverrides {
            model: Some("context-tools-updated".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Continue with updated tools.")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for (request, description) in requests.iter().zip(["Catalog context guidance.", ""]) {
        let body = request.body_json();
        for name in ["new_context", "get_context_remaining"] {
            let tool = body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            assert_eq!(tool["description"], description);
            assert_eq!(tool["parameters"]["description"], "Catalog arguments.");
        }
    }
    insta::assert_snapshot!(context_snapshot::format_request_history_snapshot(
        "Context tool descriptions and parameters use the captured model; selecting a model with explicit empty descriptions preserves empty text and prior history.",
        &requests,
        &ContextSnapshotOptions::default()
            .rewrite_known_segments()
            .include_request_settings(),
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_catalog_overrides_keep_bounded_bundled_tools_and_history() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let mock = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_completed("first")]),
            sse(vec![ev_completed("second")]),
        ],
    )
    .await;
    let test =
        test_codex()
            .with_model_info_override("gpt-5.4", |model| {
                model.use_responses_lite = true;
                model.tool_mode = Some(ToolMode::Direct);
            })
            .with_config(|config| {
                for feature in [Feature::TokenBudget, Feature::IncrementalTools] {
                    config.features.enable(feature).expect("feature");
                }
                let catalog = config.model_catalog.as_mut().expect("model catalog");
                let mut updated = catalog
                    .models
                    .iter()
                    .find(|model| model.slug == "gpt-5.4")
                    .unwrap()
                    .clone();
                updated.slug = "oversized-context-tools".to_string();
                let message = ToolMessage {
                description: Some("x".repeat(/*n*/ 100_000)),
                parameters: Some(serde_json::json!({
                    "type": "object", "properties": {}, "description": "y".repeat(/*n*/ 100_000)
                }).to_string()),
            };
                updated
                    .model_messages
                    .get_or_insert_default()
                    .tools
                    .get_or_insert_default()
                    .context_management = Some(ContextManagementToolMessages {
                    new_context: Some(message.clone()),
                    get_context_remaining: Some(message),
                });
                catalog.models.push(updated);
            })
            .build_with_auto_env(&server)
            .await?;
    test.submit_text_turn("Begin work.").await?;
    core_test_support::submit_thread_settings(
        &test.codex,
        codex_protocol::protocol::ThreadSettingsOverrides {
            model: Some("oversized-context-tools".to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.submit_text_turn("Continue with updated tools.")
        .await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    let first = requests[0].input();
    let second = requests[1].input();
    assert_eq!(&second[..first.len()], first.as_slice());
    for request in &requests {
        for item in request.input() {
            assert!(
                serde_json::to_vec(&item)?.len() <= 40_000,
                "oversized {}",
                item["type"]
            );
        }
        let declarations = request
            .input()
            .into_iter()
            .filter(|item| item["type"] == "additional_tools")
            .flat_map(|item| item["tools"].as_array().unwrap().clone())
            .flat_map(|tool| {
                tool["tools"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![tool])
            })
            .collect::<Vec<_>>();
        for name in ["new_context", "get_context_remaining"] {
            let tool = declarations
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("context tool stays available");
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
    }
    let declarations = |request: &core_test_support::responses::ResponsesRequest| {
        request
            .input()
            .into_iter()
            .filter(|item| item["type"] == "additional_tools")
            .collect::<Vec<_>>()
    };
    assert_eq!(declarations(&requests[0]), declarations(&requests[1]));
    Ok(())
}
