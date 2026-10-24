//! Initial base instructions, recorded in history for incremental-tools windows.

use super::ContextualUserFragment;
use crate::context_manager::MAX_MODEL_CONTEXT_ITEM_TOKENS;
use crate::utils::json::serialized_json_bytes;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::input_text;
use codex_utils_string::approx_bytes_for_tokens;
use codex_utils_string::take_bytes_at_char_boundary;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BaseInstructionsFragment(pub(crate) String);

impl BaseInstructionsFragment {
    pub(crate) const KIND: &str = "model.base_instructions";

    /// Builds lossless, independently bounded messages without modifying saved history.
    pub(crate) fn into_bounded_items(
        self,
        prefix_namespace: Uuid,
    ) -> serde_json::Result<Vec<ResponseItem>> {
        let limit = approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS);
        let instruction_namespace = Uuid::new_v5(&prefix_namespace, self.0.as_bytes());
        let mut complete = ContextualUserFragment::into(self.clone());
        complete.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            instruction_namespace,
        )));
        if serialized_json_bytes(&complete)? <= limit {
            return Ok(vec![complete]);
        }

        let mut items = Vec::new();
        let mut remaining = self.0.as_str();
        let mut part = 1_u64;
        while !remaining.is_empty() {
            let id = ResponseItemId::with_suffix(
                "msg",
                Uuid::new_v5(&instruction_namespace, &part.to_be_bytes()),
            );
            let make_item = |payload: &str| {
                let mut item = ContextualUserFragment::into(BaseInstructionsPart { part, payload });
                item.set_id(Some(id.clone()));
                item
            };
            // Serialized size is monotone even between UTF-8 boundaries: flooring a
            // candidate byte offset repeats the preceding valid prefix, never skips it.
            let mut lower = 0;
            let mut upper = remaining.len().min(limit);
            while lower < upper {
                let middle = lower + (upper - lower).div_ceil(/*rhs*/ 2);
                let payload = take_bytes_at_char_boundary(remaining, middle);
                if serialized_json_bytes(&make_item(payload))? <= limit {
                    lower = middle;
                } else {
                    upper = middle - 1;
                }
            }
            let payload = take_bytes_at_char_boundary(remaining, lower);
            // The fixed framing and one scalar always fit well below the item budget.
            assert!(!payload.is_empty());
            let item = make_item(payload);
            assert!(serialized_json_bytes(&item)? <= limit);
            items.push(item);
            remaining = &remaining[payload.len()..];
            part += 1;
        }
        Ok(items)
    }

    /// Recovers the original base from consecutive, independently bounded messages.
    pub(crate) fn text_from_items<'a>(
        items: impl IntoIterator<Item = &'a ResponseItem>,
    ) -> Option<String> {
        let mut restored = String::new();
        let mut next_part = 1_u64;
        for item in items {
            let Some(text) = Self::text_from_item(item) else {
                if next_part > 1 {
                    break;
                }
                continue;
            };
            let is_part = matches!(item, ResponseItem::Message {
                internal_chat_message_metadata_passthrough: Some(metadata), ..
            } if matches!(metadata.content_item_kinds.as_deref(), Some([kind])
                if kind.as_str() == BaseInstructionsPart::KIND));
            if !is_part {
                if next_part > 1 {
                    break;
                }
                return Some(text.to_owned());
            }
            let framing = text.strip_prefix("Base instructions, part ")?;
            let (part, framing) = framing.split_once('.')?;
            if part.parse::<u64>().ok()? != next_part {
                return None;
            }
            let (_, payload) = framing.split_once("\nPayload (")?;
            let (bytes, payload) = payload.split_once(" UTF-8 bytes):\n")?;
            if bytes.parse::<usize>().ok()? != payload.len() {
                return None;
            }
            restored.push_str(payload);
            next_part += 1;
        }
        (next_part > 1).then_some(restored)
    }

    /// Reads only a dedicated, harness-classified base item, including an explicitly empty base.
    pub(crate) fn text_from_item(item: &ResponseItem) -> Option<&str> {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            return None;
        };
        match (
            role.as_str(),
            content.as_slice(),
            metadata.content_item_kinds.as_deref(),
        ) {
            ("developer", [ContentItem::InputText { text }], Some([kind]))
                if matches!(kind.as_str(), Self::KIND | BaseInstructionsPart::KIND) =>
            {
                Some(text)
            }
            _ => None,
        }
    }
}

/// Bounds only new model-instruction messages, preserving their stamped metadata and first ID.
pub(crate) fn bound_model_instruction_items(items: Vec<ResponseItem>) -> Vec<ResponseItem> {
    let limit = approx_bytes_for_tokens(MAX_MODEL_CONTEXT_ITEM_TOKENS);
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
            || !matches!(metadata.content_item_kinds.as_deref(), Some([kind])
                if matches!(kind.as_str(), BaseInstructionsFragment::KIND | "model.instructions"))
            || text.is_empty()
            || serialized_json_bytes(&item).is_ok_and(|bytes| bytes <= limit)
        {
            bounded.push(item);
            continue;
        }
        let part_kind = if metadata.content_item_kinds.as_deref()
            == Some(&[ContentItemKind(BaseInstructionsFragment::KIND.to_string())])
        {
            BaseInstructionsPart::KIND
        } else {
            "model.instructions.part"
        };
        let mut template = item;
        let ResponseItem::Message { content, .. } = &mut template else {
            unreachable!("validated model instruction message");
        };
        let Some(ContentItem::InputText { text }) = content.pop() else {
            unreachable!("validated single instruction text");
        };
        let mut remaining = text.as_str();
        let mut part = 1_u64;
        while !remaining.is_empty() {
            let id = if part == 1 {
                template.id().cloned()
            } else {
                Some(ResponseItemId::new("msg"))
            };
            let make_item = |payload| {
                let mut result = template.clone();
                result.set_id(id.clone());
                if let ResponseItem::Message {
                    content,
                    internal_chat_message_metadata_passthrough: Some(metadata),
                    ..
                } = &mut result
                {
                    *content = vec![BaseInstructionsPart { part, payload }.render()];
                    metadata.content_item_kinds =
                        Some(vec![ContentItemKind(part_kind.to_string())]);
                }
                result
            };
            let mut lower = 0;
            let mut upper = remaining.len().min(limit);
            while lower < upper {
                let middle = lower + (upper - lower).div_ceil(/*rhs*/ 2);
                let payload = take_bytes_at_char_boundary(remaining, middle);
                if serialized_json_bytes(&make_item(payload)).is_ok_and(|bytes| bytes <= limit) {
                    lower = middle;
                } else {
                    upper = middle - 1;
                }
            }
            let payload = take_bytes_at_char_boundary(remaining, lower);
            assert!(
                !payload.is_empty(),
                "model instruction framing exceeds item budget"
            );
            bounded.push(make_item(payload));
            remaining = &remaining[payload.len()..];
            part += 1;
        }
    }
    bounded
}

impl ContextualUserFragment for BaseInstructionsFragment {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(Self::KIND.to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        input_text(self.0.clone())
    }
}

/// A consecutive, lossless part of instructions that exceed one serialized item.
struct BaseInstructionsPart<'a> {
    part: u64,
    payload: &'a str,
}

impl BaseInstructionsPart<'_> {
    const KIND: &str = "model.base_instructions.part";
}

impl ContextualUserFragment for BaseInstructionsPart<'_> {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(Self::KIND.to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> ContentItem {
        let Self { part, payload } = self;
        let bytes = payload.len();
        input_text(format!(
            "Base instructions, part {part}. Concatenate consecutive payloads without separators to obtain the complete instructions.\nPayload ({bytes} UTF-8 bytes):\n{payload}"
        ))
    }
}

#[cfg(test)]
#[path = "base_instructions_tests.rs"]
mod tests;
