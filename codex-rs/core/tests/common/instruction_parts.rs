//! Decode lossless instruction continuations for logical request assertions and snapshot selection.

use regex_lite::Regex;
use serde_json::Value;
use std::sync::OnceLock;

const SWITCH_PREFIX: &str = "<model_switch>\nThe user was previously using a different model. Please continue the conversation according to the following instructions:\n\n";

pub(crate) fn prefix(items: &[Value]) -> (usize, String) {
    let start = usize::from(
        items
            .first()
            .is_some_and(|item| item["type"] == "additional_tools"),
    );
    let Some(item) = items.get(start) else {
        return (0, String::new());
    };
    if item["type"] != "message" || item["role"] != "developer" {
        return (0, String::new());
    }
    if let Some((count, text)) = logical(&items[start..]) {
        return (start + count, text);
    }
    (
        start + 1,
        item["content"][0]["text"]
            .as_str()
            .expect("base instructions text")
            .to_string(),
    )
}

pub(crate) fn rebuilt_prefix_len(items: &[Value]) -> usize {
    // The client rebuilds v5 thread-scoped IDs; stamped conversation items use v7 IDs.
    static REBUILT_ID: OnceLock<Regex> = OnceLock::new();
    let rebuilt = |item: &Value, prefix: &str| {
        item["internal_chat_message_metadata_passthrough"]["turn_id"].is_null()
            && item["internal_chat_message_metadata_passthrough"]["create_time"].is_null()
            && item["id"].as_str().is_some_and(|id| id.starts_with(prefix) && REBUILT_ID.get_or_init(|| Regex::new(r"^(msg|at)_[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").expect("rebuilt ID regex")).is_match(id))
    };
    let start = usize::from(
        items
            .first()
            .is_some_and(|item| item["type"] == "additional_tools" && rebuilt(item, "at_")),
    );
    let Some(item) = items.get(start) else {
        return start;
    };
    if item["type"] != "message"
        || item["role"] != "developer"
        || !item["content"].as_array().is_some_and(|content| {
            content.len() == 1
                && content[0]["type"] == "input_text"
                && content[0]["text"].is_string()
        })
        || !rebuilt(item, "msg_")
    {
        return start;
    }
    let (count, _) = prefix(&items[start..]);
    if items[start..start + count]
        .iter()
        .all(|item| rebuilt(item, "msg_"))
    {
        start + count
    } else {
        start
    }
}

pub(crate) fn logical(items: &[Value]) -> Option<(usize, String)> {
    let (count, text) = decode(items)?;
    (count > 1 || matches!(items[0]["internal_chat_message_metadata_passthrough"]["content_item_kinds"].as_array().map(Vec::as_slice), Some([kind]) if kind == "model.base_instructions.part" || kind == "model.instructions.part")).then_some((count, text))
}

pub(crate) fn decode(items: &[Value]) -> Option<(usize, String)> {
    let mut restored = String::new();
    let mut count = 0;
    let mut wrapped = None;
    for item in items {
        let Some(content) = item["content"].as_array() else {
            break;
        };
        if item["type"] != "message"
            || item["role"] != "developer"
            || content.len() != 1
            || content[0]["type"] != "input_text"
        {
            break;
        }
        let text = content[0]["text"].as_str()?;
        let (text, is_wrapped) = match text.strip_prefix(SWITCH_PREFIX) {
            Some(text) => (text.strip_suffix("\n</model_switch>")?, true),
            None => (text, false),
        };
        let Some(framing) = text.strip_prefix("Base instructions, part ") else {
            break;
        };
        let (ordinal, framing) = framing.split_once(". Concatenate consecutive payloads without separators to obtain the complete instructions.\nPayload (")?;
        if ordinal == "1" && count > 0 {
            break;
        }
        if ordinal != (count + 1).to_string()
            || wrapped.is_some_and(|previous| previous != is_wrapped)
        {
            return None;
        }
        let kinds = &item["internal_chat_message_metadata_passthrough"]["content_item_kinds"];
        if !kinds.is_null()
            && !matches!(kinds.as_array()?.as_slice(), [kind] if kind == "model.base_instructions.part" || kind == "model.instructions.part")
        {
            return None;
        }
        let (bytes, payload) = framing.split_once(" UTF-8 bytes):\n")?;
        if bytes != payload.len().to_string() {
            return None;
        }
        restored.push_str(payload);
        count += 1;
        wrapped = Some(is_wrapped);
    }
    (count > 0).then_some((count, restored))
}

#[cfg(test)]
#[path = "instruction_parts_tests.rs"]
mod tests;
