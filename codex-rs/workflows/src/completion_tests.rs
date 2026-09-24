use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

fn schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "action": {
                "description": "Action to run.",
                "enum": ["review", "report"]
            },
            "targetRef": {
                "description": "Target revision.",
                "type": "string"
            }
        }
    })
}

#[test]
fn derives_field_and_enum_completions_from_schema() {
    let fields = static_completions(
        &schema(),
        &CompletionRequest {
            input: json!({}),
            active_field: None,
            prefix: "--t".to_string(),
            mode: CompletionMode::Field,
        },
    );
    assert_eq!(
        items(fields),
        vec![CompletionItem {
            value: "--target-ref".to_string(),
            description: Some("Target revision.".to_string()),
        }]
    );

    let values = static_completions(
        &schema(),
        &CompletionRequest {
            input: json!({}),
            active_field: Some("action".to_string()),
            prefix: "re".to_string(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        items(values),
        vec![
            CompletionItem {
                value: "review".to_string(),
                description: Some("Action to run.".to_string()),
            },
            CompletionItem {
                value: "report".to_string(),
                description: Some("Action to run.".to_string()),
            },
        ]
    );
}

#[test]
fn merges_and_deduplicates_dynamic_completions() {
    assert_eq!(
        merge_completions(
            vec![CompletionCandidate {
                item: CompletionItem {
                    value: "review".to_string(),
                    description: None,
                },
                insertion: "review".to_string(),
            }],
            vec![
                CompletionItem {
                    value: "review".to_string(),
                    description: Some("Dynamic description".to_string()),
                },
                CompletionItem {
                    value: "repair".to_string(),
                    description: None,
                },
            ],
        ),
        candidates(vec![
            CompletionItem {
                value: "repair".to_string(),
                description: None,
            },
            CompletionItem {
                value: "review".to_string(),
                description: Some("Dynamic description".to_string()),
            },
        ])
    );
}

#[test]
fn preserves_json_types_for_schema_values_and_honors_const() {
    let schema = json!({
        "type": "object",
        "properties": {
            "choice": {
                "const": "true",
                "enum": ["true", "ignored"]
            },
            "shape": {
                "enum": [null, [1, 2], {"kind": "report"}]
            }
        }
    });
    let string_candidates = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("choice".to_string()),
            prefix: String::new(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(string_candidates.len(), 1);
    assert_eq!(string_candidates[0].item.value, "true");
    assert_eq!(string_candidates[0].insertion, r#""true""#);

    let structured = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("shape".to_string()),
            prefix: String::new(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        structured
            .into_iter()
            .map(|candidate| (candidate.item.value, candidate.insertion))
            .collect::<Vec<_>>(),
        vec![
            ("null".to_string(), "null".to_string()),
            ("[1,2]".to_string(), "[1,2]".to_string()),
            (
                r#"{"kind":"report"}"#.to_string(),
                r#"{"kind":"report"}"#.to_string(),
            ),
        ]
    );
}

#[test]
fn derives_fields_and_values_through_local_refs_and_compositions() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "base": {
                "type": "object",
                "properties": {
                    "action": {
                        "$ref": "#/$defs/action"
                    }
                }
            },
            "action": {
                "description": "Action to run.",
                "anyOf": [
                    { "enum": ["review", "report"] },
                    { "const": "repair" }
                ]
            }
        },
        "allOf": [
            { "$ref": "#/$defs/base" }
        ],
        "oneOf": [
            {
                "properties": {
                    "targetRef": {
                        "description": "Target revision.",
                        "const": "main"
                    }
                }
            },
            {
                "properties": {
                    "targetRef": {
                        "description": "Target revision.",
                        "enum": ["next"]
                    }
                }
            }
        ]
    });
    let fields = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: None,
            prefix: String::new(),
            mode: CompletionMode::Field,
        },
    );
    assert_eq!(
        items(fields),
        vec![
            CompletionItem {
                value: "--action".to_string(),
                description: Some("Action to run.".to_string()),
            },
            CompletionItem {
                value: "--target-ref".to_string(),
                description: Some("Target revision.".to_string()),
            },
        ]
    );

    let action_values = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("action".to_string()),
            prefix: "re".to_string(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        items(action_values),
        vec![
            CompletionItem {
                value: "review".to_string(),
                description: Some("Action to run.".to_string()),
            },
            CompletionItem {
                value: "report".to_string(),
                description: Some("Action to run.".to_string()),
            },
            CompletionItem {
                value: "repair".to_string(),
                description: Some("Action to run.".to_string()),
            },
        ]
    );

    let target_values = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("targetRef".to_string()),
            prefix: String::new(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        items(target_values),
        vec![
            CompletionItem {
                value: "main".to_string(),
                description: Some("Target revision.".to_string()),
            },
            CompletionItem {
                value: "next".to_string(),
                description: Some("Target revision.".to_string()),
            },
        ]
    );
}

#[test]
fn intersects_all_of_values_and_omits_conflicting_descriptions() {
    let schema = json!({
        "type": "object",
        "allOf": [
            {
                "properties": {
                    "action": {
                        "description": "First description.",
                        "enum": ["review", "report"]
                    }
                }
            },
            {
                "properties": {
                    "action": {
                        "description": "Conflicting description.",
                        "enum": ["report", "repair"]
                    }
                }
            }
        ]
    });
    let fields = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: None,
            prefix: String::new(),
            mode: CompletionMode::Field,
        },
    );
    assert_eq!(
        items(fields),
        vec![CompletionItem {
            value: "--action".to_string(),
            description: None,
        }]
    );
    let values = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("action".to_string()),
            prefix: String::new(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        items(values),
        vec![CompletionItem {
            value: "report".to_string(),
            description: None,
        }]
    );
}

#[test]
fn local_ref_cycles_are_bounded_and_keep_direct_properties() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "cycle": {
                "$ref": "#/$defs/cycle",
                "properties": {
                    "action": { "const": "review" }
                }
            }
        },
        "$ref": "#/$defs/cycle"
    });
    let values = static_completions(
        &schema,
        &CompletionRequest {
            input: json!({}),
            active_field: Some("action".to_string()),
            prefix: String::new(),
            mode: CompletionMode::Value,
        },
    );
    assert_eq!(
        items(values),
        vec![CompletionItem {
            value: "review".to_string(),
            description: None,
        }]
    );
}

fn items(candidates: Vec<CompletionCandidate>) -> Vec<CompletionItem> {
    candidates
        .into_iter()
        .map(|candidate| candidate.item)
        .collect()
}

fn candidates(items: Vec<CompletionItem>) -> Vec<CompletionCandidate> {
    items
        .into_iter()
        .map(|item| CompletionCandidate {
            insertion: item.value.clone(),
            item,
        })
        .collect()
}
