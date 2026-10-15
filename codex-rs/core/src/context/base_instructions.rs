use super::ContextualUserFragment;
use crate::context_manager::MAX_MODEL_CONTEXT_ITEM_TOKENS;
use crate::utils::json::serialized_json_bytes;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
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

    pub(crate) fn matches_item(item: &ResponseItem) -> bool {
        let ResponseItem::Message {
            internal_chat_message_metadata_passthrough: Some(metadata),
            ..
        } = item
        else {
            return false;
        };
        metadata
            .content_item_kinds
            .as_ref()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Self::KIND))
    }
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

    fn body(&self) -> String {
        self.0.clone()
    }
}

/// A consecutive, lossless part of instructions that exceed one serialized item.
struct BaseInstructionsPart<'a> {
    part: u64,
    payload: &'a str,
}

impl ContextualUserFragment for BaseInstructionsPart<'_> {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(BaseInstructionsFragment::KIND.to_string())
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

    fn body(&self) -> String {
        let Self { part, payload } = self;
        let bytes = payload.len();
        format!(
            "Base instructions, part {part}. Concatenate consecutive payloads without separators to obtain the complete instructions.\nPayload ({bytes} UTF-8 bytes):\n{payload}"
        )
    }
}

#[cfg(test)]
#[path = "base_instructions_tests.rs"]
mod tests;
