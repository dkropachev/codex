//! Bounds newly appended tool instruction text after history IDs and metadata are present.
//! Original structured tool declarations and already retained history are never partitioned.

use crate::context::ContextualUserFragment;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::ContentItemNamespace;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::input_text;
use codex_utils_string::take_bytes_at_char_boundary;

pub(super) const MAX_ITEM_BYTES: usize = 40_000;
const TEXT_ITEM_BYTES: usize = 4_000;
const CONTENT_KIND: &str = "tools.namespace_instructions";

pub(super) struct ToolInstructions {
    text: String,
    part: Option<(usize, usize)>,
    namespace: ContentItemNamespace,
}

impl ToolInstructions {
    pub(super) fn new(text: String, namespace: ContentItemNamespace) -> Self {
        Self {
            text,
            part: None,
            namespace,
        }
    }
}

impl ContextualUserFragment for ToolInstructions {
    fn role(&self) -> &'static str {
        "developer"
    }
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(CONTENT_KIND.to_string())
    }
    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }
    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
    fn content_metadata(&self) -> ContentItemMetadata {
        ContentItemMetadata::tool(Some(self.namespace.clone()))
    }
    fn body(&self) -> ContentItem {
        let Some((part, parts)) = self.part else {
            return input_text(self.text.clone());
        };
        let payload = &self.text;
        let bytes = payload.len();
        // Fixed-width counts make sizing independent of the final number of parts.
        input_text(format!(
            "Tool namespace instructions, part {part:020}/{parts:020}. Join these {parts:020} consecutive payloads without separators to recover the complete instruction.\nPayload ({bytes:020} UTF-8 bytes):\n{payload}"
        ))
    }
}

fn serialized_bytes(item: &ResponseItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

fn part_item(template: &ResponseItem, text: &str, part: usize, parts: usize) -> ResponseItem {
    let mut item = template.clone();
    if let ResponseItem::Message { content, .. } = &mut item {
        *content = vec![ContentItem::InputText {
            text: ToolInstructions {
                text: text.to_string(),
                part: Some((part, parts)),
                namespace: ContentItemNamespace::Functions,
            }
            .render()
            .into_text()
            .unwrap_or_default(),
        }];
    }
    item
}

/// Finalizes only fresh typed tool instructions, after stamping and before their first append.
pub(crate) fn bound_tool_instruction_items(items: Vec<ResponseItem>) -> Vec<ResponseItem> {
    let mut bounded = Vec::new();
    for item in items {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = &item
        else {
            bounded.push(item);
            continue;
        };
        let [ContentItem::InputText { text }] = content.as_slice() else {
            bounded.push(item);
            continue;
        };
        if role != "developer"
            || metadata.content_item_kinds.as_deref()
                != Some(&[ContentItemKind(CONTENT_KIND.to_string())])
            || text.is_empty()
            || serialized_bytes(&item) <= TEXT_ITEM_BYTES
        {
            bounded.push(item);
            continue;
        }
        let mut template = item.clone();
        if let ResponseItem::Message { content, .. } = &mut template {
            content.clear();
        }
        let largest_character = text
            .chars()
            .max_by_key(|character| match character {
                '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
                character if *character < '\u{20}' => 6,
                character => character.len_utf8(),
            })
            .map(|character| character.to_string())
            .unwrap_or_default();
        let minimum = part_item(
            &template,
            &largest_character,
            /*part*/ 1,
            /*parts*/ 1,
        );
        let minimum_bytes = serialized_bytes(&minimum);
        let limit = if minimum_bytes <= TEXT_ITEM_BYTES {
            TEXT_ITEM_BYTES
        } else if serialized_bytes(&item) <= MAX_ITEM_BYTES {
            // Preexisting large metadata can exceed the preferred 1K-token text target.
            bounded.push(item);
            continue;
        } else if minimum_bytes <= MAX_ITEM_BYTES {
            MAX_ITEM_BYTES
        } else {
            // An independently oversized metadata envelope cannot be repaired by splitting text.
            tracing::warn!(
                minimum_bytes,
                "tool instruction metadata leaves no room for a continuation"
            );
            bounded.push(item);
            continue;
        };
        let mut remaining = text.as_str();
        let mut payloads = Vec::new();
        while !remaining.is_empty() {
            let mut low = remaining
                .chars()
                .next()
                .map_or(/*default*/ 0, char::len_utf8);
            let mut high = remaining.len().min(limit);
            while low < high {
                let middle = low + (high - low).div_ceil(/*rhs*/ 2);
                let payload = take_bytes_at_char_boundary(remaining, middle);
                if serialized_bytes(&part_item(
                    &template, payload, /*part*/ 1, /*parts*/ 1,
                )) <= limit
                {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            let payload = take_bytes_at_char_boundary(remaining, low);
            payloads.push(payload);
            remaining = &remaining[payload.len()..];
        }
        for (index, payload) in payloads.iter().enumerate() {
            let mut part = part_item(&template, payload, index + 1, payloads.len());
            if index != 0 {
                part.set_id(Some(ResponseItemId::new("msg")));
            }
            debug_assert!(serialized_bytes(&part) <= limit);
            bounded.push(part);
        }
    }
    bounded
}

#[cfg(test)]
#[path = "tool_instructions_tests.rs"]
mod tests;
