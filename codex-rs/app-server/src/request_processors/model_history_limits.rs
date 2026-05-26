use codex_app_server_protocol::JSONRPCErrorError;
use codex_protocol::models::ResponseItem;
use serde::Serialize;
use serde_json::Value as JsonValue;

use super::internal_error;
use super::invalid_request;

const MAX_MODEL_VISIBLE_HISTORY_ITEMS: usize = 256;
const MAX_MODEL_VISIBLE_HISTORY_ITEM_BYTES: usize = 40_000;
const MAX_MODEL_VISIBLE_HISTORY_TOTAL_BYTES: usize = 1_000_000;

pub(crate) fn validate_model_visible_response_items(
    field_name: &str,
    items: &[ResponseItem],
) -> Result<(), JSONRPCErrorError> {
    validate_model_visible_items(field_name, items)
}

pub(crate) fn validate_model_visible_json_values(
    field_name: &str,
    items: &[JsonValue],
) -> Result<(), JSONRPCErrorError> {
    validate_model_visible_items(field_name, items)
}

fn validate_model_visible_items<T: Serialize>(
    field_name: &str,
    items: &[T],
) -> Result<(), JSONRPCErrorError> {
    if items.len() > MAX_MODEL_VISIBLE_HISTORY_ITEMS {
        return Err(invalid_request(format!(
            "{field_name} must contain at most {MAX_MODEL_VISIBLE_HISTORY_ITEMS} items"
        )));
    }

    let mut total_bytes = 0usize;
    for (index, item) in items.iter().enumerate() {
        let item_bytes = serde_json::to_vec(item)
            .map_err(|err| internal_error(format!("failed to measure {field_name}: {err}")))?
            .len();
        if item_bytes > MAX_MODEL_VISIBLE_HISTORY_ITEM_BYTES {
            return Err(invalid_request(format!(
                "{field_name}[{index}] exceeds the maximum serialized size of {MAX_MODEL_VISIBLE_HISTORY_ITEM_BYTES} bytes"
            )));
        }
        total_bytes = total_bytes.saturating_add(item_bytes);
        if total_bytes > MAX_MODEL_VISIBLE_HISTORY_TOTAL_BYTES {
            return Err(invalid_request(format!(
                "{field_name} exceeds the maximum serialized size of {MAX_MODEL_VISIBLE_HISTORY_TOTAL_BYTES} bytes"
            )));
        }
    }

    Ok(())
}
