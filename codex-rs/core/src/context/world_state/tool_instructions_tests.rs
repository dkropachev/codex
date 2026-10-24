use super::*;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use pretty_assertions::assert_eq;
use std::collections::HashSet;

#[test]
fn continuations_preserve_escaped_unicode_payload_metadata_and_identity() {
    let text = "prefix\n\"\\\0é🦀suffix".repeat(/*n*/ 5_000);
    for turn_id_bytes in [36, 38_900, 39_600, 40_100] {
        let mut original = ContextualUserFragment::into(ToolInstructions::new(
            text.clone(),
            codex_protocol::models::ContentItemNamespace::Functions,
        ));
        original.set_id(Some(ResponseItemId::new("msg")));
        if let ResponseItem::Message {
            internal_chat_message_metadata_passthrough: metadata,
            ..
        } = &mut original
        {
            *metadata = Some(InternalChatMessageMetadataPassthrough {
                turn_id: Some("t".repeat(turn_id_bytes)),
                create_time: Some(serde_json::Number::from(/*i*/ 1_792_098_000)),
                content_item_kinds: Some(vec![ContentItemKind(CONTENT_KIND.to_string())]),
                ..Default::default()
            });
        }
        let parts = bound_tool_instruction_items(vec![original.clone()]);
        if parts.len() == 1 && serialized_bytes(&parts[0]) > MAX_ITEM_BYTES {
            // The independently oversized metadata/framing case must terminate losslessly.
            assert_eq!(parts, vec![original]);
            continue;
        }
        assert_eq!(parts[0].id(), original.id());
        let mut ids = HashSet::new();
        let mut reconstructed = String::new();
        for part in &parts {
            assert!(ids.insert(part.id().expect("part ID")));
            assert!(
                serialized_bytes(part)
                    <= if turn_id_bytes == 36 {
                        TEXT_ITEM_BYTES
                    } else {
                        MAX_ITEM_BYTES
                    }
            );
            let ResponseItem::Message {
                content,
                internal_chat_message_metadata_passthrough: metadata,
                ..
            } = part
            else {
                panic!("message")
            };
            let ResponseItem::Message {
                internal_chat_message_metadata_passthrough: original_metadata,
                ..
            } = &original
            else {
                panic!("message")
            };
            assert_eq!(metadata, original_metadata);
            let ContentItem::InputText { text } = &content[0] else {
                panic!("text")
            };
            reconstructed.push_str(
                text.split_once(" UTF-8 bytes):\n")
                    .expect("payload frame")
                    .1,
            );
        }
        assert_eq!(reconstructed, text);
        // Finalization is idempotent: already bounded parts keep their IDs and full envelopes.
        assert_eq!(bound_tool_instruction_items(parts.clone()), parts);
    }
}
