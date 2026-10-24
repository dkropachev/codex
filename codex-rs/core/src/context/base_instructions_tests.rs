use super::*;
use codex_protocol::models::BaseInstructions;
use pretty_assertions::assert_eq;

#[test]
fn fitting_items_preserve_complete_wire_representation_and_identity() -> serde_json::Result<()> {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"thread");
    let overhead = {
        let mut item = ContextualUserFragment::into(BaseInstructionsFragment(String::new()));
        item.set_id(Some(ResponseItemId::with_suffix("msg", namespace)));
        serialized_json_bytes(&item)?
    };
    let limit = approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS);
    for text in [
        BaseInstructions::default().text,
        "x".repeat(/*n*/ limit - overhead),
    ] {
        let mut expected = ContextualUserFragment::into(BaseInstructionsFragment(text.clone()));
        expected.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            Uuid::new_v5(&namespace, text.as_bytes()),
        )));
        assert_eq!(
            BaseInstructionsFragment(text).into_bounded_items(namespace)?,
            vec![expected]
        );
    }
    Ok(())
}

#[test]
fn bounded_history_preserves_complete_instructions_metadata_and_identity() -> serde_json::Result<()>
{
    let text = "Quoted Unicode: \"診断\"\n".repeat(8_000);
    let mut original = ContextualUserFragment::into(BaseInstructionsFragment(text.clone()));
    original.set_id(Some(ResponseItemId::new("msg")));
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = &mut original
    {
        metadata.turn_id = Some("turn-1".to_string());
    }
    let parts = bound_model_instruction_items(vec![original.clone()]);
    assert!(parts.len() > 1);
    assert_eq!(parts[0].id(), original.id());
    assert_eq!(
        parts
            .iter()
            .filter_map(ResponseItem::id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        parts.len(),
    );
    for part in &parts {
        assert!(
            serialized_json_bytes(part)? <= approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS)
        );
        let mut restored = part.clone();
        if let ResponseItem::Message {
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = &mut restored
        {
            *content = vec![input_text(text.clone())];
            metadata.content_item_kinds = Some(vec![ContentItemKind(
                BaseInstructionsFragment::KIND.to_string(),
            )]);
        }
        restored.set_id(original.id().cloned());
        assert_eq!(restored, original);
    }
    assert_eq!(
        BaseInstructionsFragment::text_from_items(&parts),
        Some(text)
    );
    assert_eq!(bound_model_instruction_items(parts.clone()), parts);
    assert_eq!(BaseInstructionsFragment::text_from_items(&parts[1..]), None);
    Ok(())
}

#[test]
fn raw_framing_text_remains_literal_and_model_updates_are_bounded() -> serde_json::Result<()> {
    let text = "Base instructions, part 1. Literal user-configured instructions.";
    let item = ContextualUserFragment::into(BaseInstructionsFragment(text.to_string()));
    assert_eq!(
        BaseInstructionsFragment::text_from_items([&item]),
        Some(text.to_string())
    );
    assert_eq!(
        bound_model_instruction_items(vec![item.clone()]),
        vec![item]
    );

    let update = ContextualUserFragment::into(crate::context::ModelInstructions {
        model: Some("new-model".to_string()),
        instructions: Some("Follow all updated instructions. ".repeat(8_000)),
    });
    let parts = bound_model_instruction_items(vec![update]);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(
            serialized_json_bytes(part)? <= approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS)
        );
    }
    assert_eq!(bound_model_instruction_items(parts.clone()), parts);
    Ok(())
}
