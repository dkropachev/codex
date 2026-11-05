//! Keeps structured declarations intact while bounding each serialized context item.

use super::tool_instructions::MAX_ITEM_BYTES;
use codex_protocol::ResponseItemId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result;
use codex_protocol::models::ResponseItem;
use serde_json::Value;
use std::sync::Arc;

#[expect(
    clippy::expect_used,
    reason = "AdditionalTools has an ID prefix and serializes infallibly"
)]
fn declaration_budget() -> usize {
    let mut envelope = ResponseItem::AdditionalTools {
        id: None,
        role: "developer".to_string(),
        tools: Arc::from([]),
    };
    let prefix = envelope.id_prefix().expect("tool declaration ID prefix");
    envelope.set_id(Some(ResponseItemId::with_suffix(prefix, uuid::Uuid::nil())));
    // Reserve the exact envelope and generated UUID length, keeping the array brackets.
    MAX_ITEM_BYTES
        - serde_json::to_vec(&envelope)
            .expect("tool declaration envelope")
            .len()
        + 2
}

pub(super) fn namespace_instructions_need_text(definition: &Value) -> bool {
    if !definition["tools"].is_array()
        || definition["description"].as_str().is_none_or(str::is_empty)
    {
        return false;
    }
    // Carry guidance once when the whole namespace cannot fit one item, instead
    // of repeating it in every batch of members.
    definition.to_string().len() + 2 > declaration_budget()
}

#[expect(
    clippy::expect_used,
    reason = "the Responses Lite serializer emits namespace objects"
)]
pub(super) fn declaration_batches(definitions: &[Value]) -> Result<Vec<Vec<Value>>> {
    let budget = declaration_budget();
    let mut batches = Vec::new();
    let mut batch: Vec<Value> = Vec::new();
    let mut bytes = 2;
    for definition in definitions {
        // Keep a fitting namespace intact so a preceding batch cannot force its
        // guidance to be repeated across member fragments.
        let declarations = if let Some(members) = definition["tools"].as_array()
            && definition.to_string().len() + 2 > budget
        {
            let mut header = definition.clone();
            header
                .as_object_mut()
                .expect("serialized namespace")
                .remove("tools");
            // Large namespace instructions are emitted as bounded text fragments.
            if namespace_instructions_need_text(definition) {
                header["description"] = Value::String(String::new());
            }
            members
                .iter()
                .map(|member| {
                    let mut namespace = header.clone();
                    namespace["tools"] = Value::Array(vec![member.clone()]);
                    namespace
                })
                .collect::<Vec<_>>()
        } else {
            vec![definition.clone()]
        };
        for declaration in declarations {
            let size = declaration.to_string().len();
            if size + 2 > budget {
                return Err(CodexErrorDetails::InvalidRequest(format!(
                    "Tool declaration exceeds the {budget}-byte context budget"
                ))
                .into());
            }
            let merged_member_bytes = batch
                .last()
                .filter(|previous| {
                    declaration["type"] == "namespace"
                        && previous["type"] == "namespace"
                        && previous["name"] == declaration["name"]
                })
                .map(|_| declaration["tools"][0].to_string().len() + 1);
            let added_bytes = merged_member_bytes.unwrap_or(size + usize::from(!batch.is_empty()));
            if bytes + added_bytes > budget {
                batches.push(std::mem::take(&mut batch));
                bytes = 2;
            }
            let separator_bytes = usize::from(!batch.is_empty());
            // Keep each namespace's members together when they fit in the same batch.
            if let Some(previous) = batch.last_mut()
                && declaration["type"] == "namespace"
                && previous["type"] == "namespace"
                && previous["name"] == declaration["name"]
                && let Some(members) = previous["tools"].as_array_mut()
            {
                members.push(declaration["tools"][0].clone());
                bytes += declaration["tools"][0].to_string().len() + 1;
            } else {
                batch.push(declaration);
                bytes += separator_bytes + size;
            }
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}
