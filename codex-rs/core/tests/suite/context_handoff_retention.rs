use anyhow::Context;
use anyhow::Result;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;

#[test_case(false; "feature_disabled")]
#[test_case(true; "feature_enabled")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_handoff_retains_client_developer_messages_when_enabled(
    enabled: bool,
) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call(
                    "checkpoint",
                    "handoff_context",
                    r#"{"summary":"Continue the migration."}"#,
                ),
                ev_completed("checkpoint"),
            ]),
            sse(vec![
                ev_assistant_message("done", "Migration complete."),
                ev_completed("done"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(move |config| {
            config.features.enable(Feature::HandoffContext).unwrap();
            config
                .features
                .disable(Feature::RetainClientDeveloperMessages)
                .unwrap();
            if enabled {
                config
                    .features
                    .enable(Feature::RetainClientDeveloperMessages)
                    .unwrap();
            }
        })
        .build_with_auto_env(&server)
        .await?;
    let developer: ResponseItem = serde_json::from_value(json!({
        "type": "message",
        "role": "developer",
        "content": [{"type": "input_text", "text": "CLIENT_DEVELOPER_INSTRUCTIONS"}]
    }))?;
    test.codex.inject_response_items(vec![developer]).await?;
    test.submit_turn("UNIQUE_OLD_USER_INSTRUCTION").await?;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].body_contains_text("CLIENT_DEVELOPER_INSTRUCTIONS"));
    assert_eq!(
        requests[1].body_contains_text("CLIENT_DEVELOPER_INSTRUCTIONS"),
        enabled
    );
    assert!(requests[1].body_contains_text("Continue the migration."));
    assert!(!requests[1].body_contains_text("UNIQUE_OLD_USER_INSTRUCTION"));

    let rollout_path = test.codex.rollout_path().context("rollout path")?;
    test.codex.shutdown_and_wait().await?;
    let rollout_items = std::fs::read_to_string(rollout_path)?
        .lines()
        .filter_map(|line| codex_rollout::parse_rollout_line(line).ok())
        .map(|line| line.item)
        .collect::<Vec<_>>();
    let original_developer = rollout_items
        .iter()
        .find_map(|item| match item {
            RolloutItem::ResponseItem(envelope)
                if matches!(&envelope.item, ResponseItem::Message { role, content, .. }
                    if role == "developer" && content.iter().any(|item|
                        matches!(item, ContentItem::InputText { text }
                            if text == "CLIENT_DEVELOPER_INSTRUCTIONS"))) =>
            {
                Some(envelope.item.clone())
            }
            _ => None,
        })
        .context("injected developer envelope")?;
    let replacement_history = rollout_items
        .into_iter()
        .filter_map(|item| match item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history,
            _ => None,
        })
        .next_back()
        .context("handoff should persist a checkpoint")?;
    let retained = replacement_history
        .into_iter()
        .filter(|item| {
            item.metadata
                .as_ref()
                .is_some_and(|metadata| metadata.client_authored)
        })
        .map(|item| item.item)
        .collect::<Vec<_>>();
    assert_eq!(
        retained,
        if enabled {
            vec![original_developer]
        } else {
            Vec::new()
        }
    );
    Ok(())
}
