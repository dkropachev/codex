//! Whole retained outputs obey the existing item estimator cap in every retention path.

use super::*;
use codex_protocol::models::FunctionCallOutputPayload;
use pretty_assertions::assert_eq;

enum RetainedBudgetPath {
    ImagesEnabled,
    ImagesDisabled,
    FreshWindow,
}

enum OversizedOutputField {
    Name,
    Namespace,
}

fn retained_output(name: &str, namespace: Option<&str>) -> ResponseItemEnvelope {
    ResponseItemEnvelope {
        item: ResponseItem::FunctionCallOutput {
            id: None,
            call_id: None,
            name: Some(name.to_string()),
            namespace: namespace.map(str::to_string),
            output: FunctionCallOutputPayload::from_text(String::new()),
            internal_chat_message_metadata_passthrough: None,
        },
        metadata: Some(CodexHarnessMetadata {
            client_authored: true,
            ..Default::default()
        }),
    }
}

#[test_case::test_case(RetainedBudgetPath::ImagesEnabled, OversizedOutputField::Name; "images enabled oversized name")]
#[test_case::test_case(RetainedBudgetPath::ImagesEnabled, OversizedOutputField::Namespace; "images enabled oversized namespace")]
#[test_case::test_case(RetainedBudgetPath::ImagesDisabled, OversizedOutputField::Name; "images disabled oversized name")]
#[test_case::test_case(RetainedBudgetPath::ImagesDisabled, OversizedOutputField::Namespace; "images disabled oversized namespace")]
#[test_case::test_case(RetainedBudgetPath::FreshWindow, OversizedOutputField::Name; "fresh window oversized name")]
#[test_case::test_case(RetainedBudgetPath::FreshWindow, OversizedOutputField::Namespace; "fresh window oversized namespace")]
fn retained_tool_outputs_enforce_item_cap_in_all_budget_paths(
    path: RetainedBudgetPath,
    field: OversizedOutputField,
) {
    // Release-mode admission can leave oversized metadata after emptying the body.
    // Exercise retention directly because debug admission rejects this legacy weakness.
    let oversized_metadata = "x".repeat(/*n*/ 50_000);
    let oversized = match field {
        OversizedOutputField::Name => retained_output(&oversized_metadata, /*namespace*/ None),
        OversizedOutputField::Namespace => retained_output("metadata", Some(&oversized_metadata)),
    };
    assert!(is_retained_tool_output(&oversized));
    let estimated_tokens = usize::try_from(estimate_item_token_count(&oversized.item))
        .expect("nonnegative output estimate");
    assert!(estimated_tokens > MAX_MODEL_CONTEXT_ITEM_TOKENS);
    assert!(estimated_tokens < RETAINED_MESSAGE_TOKEN_BUDGET);
    let newest = retained_output("newest", /*namespace*/ None);
    let items = vec![
        retained_output("older", /*namespace*/ None),
        oversized,
        newest.clone(),
    ];
    let retained = match path {
        RetainedBudgetPath::ImagesEnabled => truncate_retained_messages(
            items,
            RETAINED_MESSAGE_TOKEN_BUDGET,
            RetainedImageBudget::Enabled,
        ),
        RetainedBudgetPath::ImagesDisabled => truncate_retained_messages(
            items,
            RETAINED_MESSAGE_TOKEN_BUDGET,
            RetainedImageBudget::Disabled,
        ),
        RetainedBudgetPath::FreshWindow => {
            truncate_retained_messages_for_remote_compaction(items, RETAINED_MESSAGE_TOKEN_BUDGET)
        }
    };

    // Guard large values before pretty equality so a cap regression has a bounded diff.
    assert_eq!(retained.len(), 1);
    for item in &retained {
        assert!(estimate_item_token_count(&item.item) <= MAX_MODEL_CONTEXT_ITEM_TOKENS as i64);
        let ResponseItem::FunctionCallOutput {
            name, namespace, ..
        } = &item.item
        else {
            panic!("expected a retained output");
        };
        assert!(
            name.as_ref()
                .is_none_or(|name| name.len() <= MAX_MODEL_CONTEXT_ITEM_TOKENS * 4)
        );
        assert!(
            namespace
                .as_ref()
                .is_none_or(|namespace| namespace.len() <= MAX_MODEL_CONTEXT_ITEM_TOKENS * 4)
        );
    }
    // The oversized output is never partially rewritten or replaced by older items.
    assert_eq!(retained, vec![newest]);
}
