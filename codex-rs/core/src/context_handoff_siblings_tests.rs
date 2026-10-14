use super::*;
use codex_history::CodexHarnessMetadata;
use pretty_assertions::assert_eq;
use serde_json::json;

fn envelope(value: serde_json::Value) -> ResponseItemEnvelope {
    ResponseItemEnvelope::new(serde_json::from_value(value).unwrap())
}

fn checkpoint() -> Vec<ResponseItemEnvelope> {
    vec![
        envelope(
            json!({"type":"function_call", "call_id":"checkpoint", "name":"handoff_context", "arguments":"{}"}),
        ),
        envelope(json!({"type":"function_call_output", "call_id":"checkpoint", "output":"queued"})),
    ]
}

fn retain(history: &[ResponseItemEnvelope]) -> Result<Vec<ResponseItemEnvelope>, ()> {
    let mut siblings = HandoffSiblings::default();
    for envelope in history {
        if matches!(
            envelope.item,
            ResponseItem::FunctionCall { .. } | ResponseItem::CustomToolCall { .. }
        ) {
            siblings.observe(&envelope.item);
        }
    }
    siblings.complete();
    siblings.retain(history, "checkpoint")
}

#[test]
fn context_handoff_siblings_preserve_custom_pair_and_metadata() {
    let mut history = checkpoint();
    history.push(envelope(json!({"type":"custom_tool_call", "call_id":"exec", "name":"exec", "namespace":"functions", "input":"text('failed');", "id":"custom-id"})));
    let mut output = envelope(
        json!({"type":"custom_tool_call_output", "call_id":"exec", "output":"failed", "id":"output-id"}),
    );
    output.metadata = Some(CodexHarnessMetadata {
        guardian_source_order_guidance: true,
        ..Default::default()
    });
    let ResponseItem::CustomToolCallOutput {
        output: payload, ..
    } = &mut output.item
    else {
        unreachable!();
    };
    payload.success = Some(false);
    history.push(output);
    assert_eq!(retain(&history), Ok(history[2..].to_vec()));
}

#[test]
fn context_handoff_siblings_reject_incomplete_mismatched_and_duplicate_pairs() {
    let call = envelope(
        json!({"type":"function_call", "call_id":"sibling", "name":"operation", "arguments":"{}"}),
    );
    let output =
        envelope(json!({"type":"function_call_output", "call_id":"sibling", "output":"failed"}));
    for suffix in [
        vec![call.clone()],
        vec![
            call.clone(),
            envelope(
                json!({"type":"custom_tool_call_output", "call_id":"sibling", "output":"failed"}),
            ),
        ],
        vec![call.clone(), output.clone(), output.clone()],
        vec![call.clone(), output.clone(), call, output],
    ] {
        let mut history = checkpoint();
        history.extend(suffix);
        assert_eq!(retain(&history), Err(()));
    }
}

#[test]
fn context_handoff_siblings_reject_item_batch_and_count_overflow() {
    for (count, size) in [(1, 40_000), (3, 30_000), (32, 1)] {
        let mut history = checkpoint();
        for index in 0..count {
            history.push(envelope(json!({"type":"function_call", "call_id":format!("call-{index}"), "name":"operation", "arguments":"{}"})));
            history.push(envelope(json!({"type":"function_call_output", "call_id":format!("call-{index}"), "output":"x".repeat(size)})));
        }
        assert_eq!(retain(&history), Err(()));
    }
}

#[test]
fn context_handoff_siblings_reject_unsupported_and_unfinished_response() {
    let history = checkpoint();
    let mut siblings = HandoffSiblings::default();
    siblings.observe(&history[0].item);
    assert_eq!(siblings.retain(&history, "checkpoint"), Err(()));
    let mut siblings = HandoffSiblings::default();
    siblings.observe(&history[0].item);
    siblings.observe(&ResponseItem::Other);
    siblings.complete();
    assert_eq!(siblings.retain(&history, "checkpoint"), Err(()));
}

#[test]
fn context_handoff_siblings_reject_multiple_checkpoints() {
    let mut history = checkpoint();
    history.push(envelope(json!({"type":"function_call", "call_id":"another-checkpoint", "name":"handoff_context", "arguments":"{}"})));
    history.push(envelope(
        json!({"type":"function_call_output", "call_id":"another-checkpoint", "output":"queued"}),
    ));
    assert_eq!(retain(&history), Err(()));
}

#[test]
fn context_handoff_siblings_reject_summary_owned_by_another_response_call() {
    let history = checkpoint();
    let mut siblings = HandoffSiblings::default();
    siblings.observe(&history[0].item);
    siblings.complete();
    assert_eq!(siblings.retain(&history, "stale-checkpoint"), Err(()));
}

#[test]
fn context_handoff_siblings_accept_exact_item_and_batch_limits() {
    let mut history = checkpoint();
    let mut expected = Vec::new();
    for index in 0..2 {
        let mut call = envelope(
            json!({"type":"function_call", "call_id":format!("call-{index}"), "name":"operation", "arguments":""}),
        );
        call.metadata = Some(CodexHarnessMetadata {
            history_truncation_token_limit: Some(MAX_ITEM_TOKENS),
            ..Default::default()
        });
        let output = envelope(
            json!({"type":"function_call_output", "call_id":format!("call-{index}"), "output":"result"}),
        );
        let overhead = serde_json::to_string(&call.item).unwrap().len();
        let output_tokens = approx_token_count(&serde_json::to_string(&output.item).unwrap());
        let call_tokens = if index == 0 {
            MAX_ITEM_TOKENS
        } else {
            MAX_ITEM_TOKENS - 2 * output_tokens
        };
        let ResponseItem::FunctionCall { arguments, .. } = &mut call.item else {
            unreachable!();
        };
        *arguments = "x".repeat(call_tokens * 4 - overhead);
        expected.extend([call, output]);
    }
    history.extend(expected.clone());
    assert_eq!(retain(&history), Ok(expected));
}
