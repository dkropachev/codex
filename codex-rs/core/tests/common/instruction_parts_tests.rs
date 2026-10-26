use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn part(ordinal: usize, payload: &str) -> Value {
    json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": format!("Base instructions, part {ordinal}. Concatenate consecutive payloads without separators to obtain the complete instructions.\nPayload ({} UTF-8 bytes):\n{payload}", payload.len())}]})
}

#[test]
fn reconstructs_exact_payloads_and_stops_at_the_next_logical_item() {
    let first = part(/*ordinal*/ 1, "é\nPayload (4 UTF-8 bytes):\nraw");
    let second = part(/*ordinal*/ 2, "\"終");
    assert_eq!(
        decode(&[first.clone(), second.clone(), part(/*ordinal*/ 1, "next")]),
        Some((2, "é\nPayload (4 UTF-8 bytes):\nraw\"終".to_string()))
    );
    let wrapped = [first, second].map(|mut item| {
        let text = item["content"][0]["text"].as_str().unwrap();
        item["content"][0]["text"] = json!(format!("{SWITCH_PREFIX}{text}\n</model_switch>"));
        item
    });
    assert_eq!(
        decode(&wrapped),
        Some((2, "é\nPayload (4 UTF-8 bytes):\nraw\"終".to_string()))
    );
}

#[test]
fn rejects_incomplete_framing_reordered_lengths_and_foreign_carriers() {
    let first = part(/*ordinal*/ 1, "é");
    let mut noncanonical = first.clone();
    noncanonical["content"][0]["text"] = json!(
        first["content"][0]["text"]
            .as_str()
            .unwrap()
            .replace("part 1.", "part +1.")
    );
    assert_eq!(decode(&[noncanonical]), None);
    let mut invalid = vec![
        part(/*ordinal*/ 2, "é"),
        first.clone(),
        first.clone(),
        first.clone(),
        first.clone(),
        first.clone(),
    ];
    invalid[1]["content"][0]["text"] =
        json!("Base instructions, part 1.\nPayload (2 UTF-8 bytes):\né");
    invalid[2]["content"][0]["text"] = json!(
        "Base instructions, part 1. Concatenate consecutive payloads without separators to obtain the complete instructions.\nPayload (1 UTF-8 bytes):\né"
    );
    invalid[3]["role"] = json!("user");
    invalid[4]["internal_chat_message_metadata_passthrough"]["content_item_kinds"] =
        json!(["developer_instructions"]);
    invalid[5]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "input_text", "text": "neighbor"}));
    for item in invalid {
        assert_eq!(decode(&[item]), None);
    }
    assert_eq!(decode(&[first, part(/*ordinal*/ 3, "gap")]), None);
}

#[test]
fn logical_read_and_rebuilt_identity_have_distinct_contracts() {
    let mut first = part(/*ordinal*/ 1, "first");
    let literal = first["content"][0]["text"].as_str().unwrap().to_string();
    assert_eq!(prefix(&[first.clone()]), (1, literal));
    first["id"] = json!("msg_00000000-0000-5000-8000-000000000001");
    assert_eq!(rebuilt_prefix_len(&[first.clone()]), 1);
    first["internal_chat_message_metadata_passthrough"] = json!({"content_item_kinds": ["model.base_instructions.part"], "turn_id": "turn", "create_time": 1});
    assert_eq!(prefix(&[first.clone()]), (1, "first".to_string()));
    assert_eq!(rebuilt_prefix_len(&[first.clone()]), 0);
    first["internal_chat_message_metadata_passthrough"] = Value::Null;
    first["id"] = json!("msg_00000000-0000-7000-8000-000000000001");
    let tools =
        json!({"type": "additional_tools", "id": "at_00000000-0000-5000-8000-000000000001"});
    assert_eq!(rebuilt_prefix_len(std::slice::from_ref(&tools)), 1);
    for input in [
        vec![],
        vec![json!({"type": "additional_tools"})],
        vec![
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "delta"}]}),
        ],
        vec![first],
    ] {
        assert_eq!(rebuilt_prefix_len(&input), 0);
    }
}
