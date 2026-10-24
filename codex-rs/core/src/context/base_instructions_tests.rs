use super::*;
use crate::context::ModelSwitchInstructions;
use codex_protocol::models::BaseInstructions;
use codex_protocol_test_support::ContentItemTestExt;
use pretty_assertions::assert_eq;

fn part_payload(text: &str) -> &str {
    let (_, framed) = text.split_once("\nPayload (").expect("part framing");
    let (bytes, payload) = framed.split_once(" UTF-8 bytes):\n").expect("payload");
    assert_eq!(bytes.parse::<usize>().expect("byte length"), payload.len());
    payload
}

fn message_text(item: &ResponseItem) -> &str {
    let ResponseItem::Message { content, .. } = item else {
        panic!("message")
    };
    let [ContentItem::InputText { text }] = content.as_slice() else {
        panic!("text")
    };
    text
}

#[test]
fn prefix_parts_are_lossless_bounded_and_policy_distinct() -> serde_json::Result<()> {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"thread");
    for text in [BaseInstructions::default().text, "123".repeat(/*n*/ 30_001)] {
        let parts = BaseInstructionsFragment(text.clone()).into_bounded_items(namespace)?;
        assert!(parts.len() > 1);
        assert_eq!(
            BaseInstructionsFragment::text_from_items(&parts),
            Some(text.clone())
        );
        for part in &parts {
            assert!(serialized_json_bytes(part)? <= 10_000);
        }
        let old_namespace = Uuid::new_v5(&namespace, text.as_bytes());
        let old_first_part =
            ResponseItemId::with_suffix("msg", Uuid::new_v5(&old_namespace, &1_u64.to_be_bytes()));
        assert_ne!(parts[0].id(), Some(&old_first_part));
        assert_eq!(
            BaseInstructionsFragment(text).into_bounded_items(namespace)?,
            parts
        );
    }
    Ok(())
}

#[test]
fn fitting_items_preserve_complete_wire_representation_and_identity() -> serde_json::Result<()> {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"thread");
    let overhead = {
        let mut item = ContextualUserFragment::into(BaseInstructionsFragment(String::new()));
        item.set_id(Some(ResponseItemId::with_suffix("msg", namespace)));
        serialized_json_bytes(&item)?
    };
    let limit = MAX_INSTRUCTION_ITEM_BYTES;
    for text in [
        "Fitting model instructions.".to_string(),
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
    let first_stamp = serde_json::Number::from_f64(/*f*/ 1.0).unwrap();
    let later_stamp = serde_json::Number::from_f64(/*f*/ 1.234_567_890_125).unwrap();
    assert_ne!(
        serialized_json_bytes(&first_stamp)?,
        serialized_json_bytes(&later_stamp)?
    );
    let text = "Quoted Unicode: \"診断\"\n".repeat(8_000);
    let mut original = ContextualUserFragment::into(BaseInstructionsFragment(text.clone()));
    original.set_id(Some(ResponseItemId::new("msg")));
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = &mut original
    {
        metadata.turn_id = Some("turn-1".to_string());
        metadata.create_time = Some(first_stamp);
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
        assert!(serialized_json_bytes(part)? <= MAX_INSTRUCTION_ITEM_BYTES);
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
    let mut later = original.clone();
    if let ResponseItem::Message {
        internal_chat_message_metadata_passthrough: Some(metadata),
        ..
    } = &mut later
    {
        metadata.create_time = Some(later_stamp.clone());
    }
    let later_parts = bound_model_instruction_items(vec![later]);
    assert_eq!(
        parts.iter().map(message_text).collect::<Vec<_>>(),
        later_parts.iter().map(message_text).collect::<Vec<_>>()
    );
    assert_eq!(later_parts[0].id(), original.id());
    for (part, later) in parts.iter().zip(&later_parts) {
        assert!(serialized_json_bytes(later)? <= MAX_INSTRUCTION_ITEM_BYTES);
        let mut expected = part.clone();
        expected.set_id(later.id().cloned());
        if let ResponseItem::Message {
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = &mut expected
        {
            metadata.create_time = Some(later_stamp.clone());
        }
        assert_eq!(&expected, later);
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
        instructions: Some("123".repeat(/*n*/ 10_001)),
    });
    let original = update.clone();
    let parts = bound_model_instruction_items(vec![update]);
    assert!(parts.len() > 1);
    for part in &parts {
        assert!(serialized_json_bytes(part)? <= MAX_INSTRUCTION_ITEM_BYTES);
    }
    let reconstructed = parts
        .iter()
        .map(|part| part_payload(message_text(part)))
        .collect::<String>();
    let ResponseItem::Message { content, .. } = original else {
        panic!("message")
    };
    assert_eq!(content, vec![input_text(reconstructed)]);
    assert_eq!(bound_model_instruction_items(parts.clone()), parts);
    Ok(())
}

#[test]
fn model_switch_parts_keep_complete_wrappers_and_metadata() -> serde_json::Result<()> {
    let original =
        ContextualUserFragment::into(ModelSwitchInstructions::new("123".repeat(/*n*/ 10_001)));
    let parts = bound_model_instruction_items(vec![original.clone()]);
    assert!(parts.len() > 1);
    let wrapper = ModelSwitchInstructions::new("PAYLOAD")
        .render()
        .expect_text();
    let (before, after) = wrapper.split_once("PAYLOAD").unwrap();
    let mut restored = String::new();
    for part in &parts {
        assert!(serialized_json_bytes(part)? <= 10_000);
        let text = message_text(part);
        assert!(ModelSwitchInstructions::matches_text(text));
        let framed = text
            .strip_prefix(before)
            .unwrap()
            .strip_suffix(after)
            .unwrap();
        restored.push_str(part_payload(framed));
    }
    let ResponseItem::Message { content, .. } = original else {
        panic!("message")
    };
    assert_eq!(content, vec![input_text(restored)]);
    assert_eq!(bound_model_instruction_items(parts.clone()), parts);
    Ok(())
}
