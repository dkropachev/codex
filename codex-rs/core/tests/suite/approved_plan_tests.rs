//! Approved-plan fallback preserves all content through normal history admission.

use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::TruncationPolicyConfig;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;

#[test_case::test_case("a", 39_500; "ascii")]
#[test_case::test_case("é", 19_750; "unicode")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_plan_fallback_survives_the_history_payload_cap(
    unit: &str,
    repetitions: usize,
) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![responses::ev_completed("done")]),
    )
    .await;
    let test = test_codex()
        .with_model_info_override("test-gpt-5.1-codex", |model| {
            model.truncation_policy = TruncationPolicyConfig::tokens(/*limit*/ 10_000);
        })
        .build_with_auto_env(&server)
        .await?;
    let markdown = format!(
        "## Approved plan\n{}\nFinish every step.",
        unit.repeat(repetitions)
    );
    let prompt = test
        .codex
        .prepare_approved_plan(/*path*/ None, &markdown)
        .await?;
    assert_eq!(prompt, "Implement the plan.");
    assert!(mock.requests().is_empty());
    test.submit_text_turn(&prompt).await?;

    let history = test.codex.conversation_history_snapshot().await;
    let chunks = history
        .items()
        .filter_map(|item| match item {
            ResponseItem::FunctionCallOutput {
                name: Some(name),
                output,
                ..
            } if name == "proposed_plan" => output.text_content(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks.concat(), markdown);
    assert_eq!(mock.requests().len(), 1);
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
