use serde_json::Value;
use serde_json::json;

const MAX_STAGE_ITEMS: usize = 64;
const MAX_CONTEXT_RANGES: usize = 256;
const MAX_TITLE_STRING: usize = 80;
const MAX_SHORT_STRING: usize = 2 * 1024;
const MAX_BODY_STRING: usize = 8 * 1024;
const MAX_PATH_STRING: usize = 4 * 1024;

fn line_range_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "start": {"type": "integer", "minimum": 1},
            "end": {"type": "integer", "minimum": 1}
        },
        "required": ["start", "end"]
    })
}

fn code_location_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "absoluteFilePath": {"type": "string", "maxLength": MAX_PATH_STRING},
            "lineRange": line_range_schema()
        },
        "required": ["absoluteFilePath", "lineRange"]
    })
}

fn assessment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["patch is correct", "patch is incorrect", "uncertain"]
            },
            "explanation": {"type": "string", "maxLength": MAX_BODY_STRING},
            "confidenceScore": {"type": "number", "minimum": 0, "maximum": 1}
        },
        "required": ["verdict", "explanation", "confidenceScore"]
    })
}

fn discovery_finding_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "title": {"type": "string", "maxLength": MAX_TITLE_STRING},
            "body": {"type": "string", "maxLength": MAX_BODY_STRING},
            "confidenceScore": {"type": "number", "minimum": 0, "maximum": 1},
            "priority": {"type": "integer", "minimum": 0, "maximum": 3},
            "codeLocation": code_location_schema()
        },
        "required": ["title", "body", "confidenceScore", "priority", "codeLocation"]
    })
}

fn verified_finding_schema() -> Value {
    let mut schema = discovery_finding_schema();
    let properties = schema["properties"]
        .as_object_mut()
        .expect("finding properties must be an object");
    properties.insert(
        "candidateIndex".to_string(),
        json!({"type": "integer", "minimum": 0}),
    );
    properties.insert(
        "preExisting".to_string(),
        json!({"type": "string", "enum": ["true", "false", "undetermined"]}),
    );
    properties.insert(
        "preExistingFixRationale".to_string(),
        json!({"type": ["string", "null"], "maxLength": MAX_SHORT_STRING}),
    );
    schema["required"] = json!([
        "candidateIndex",
        "title",
        "body",
        "confidenceScore",
        "priority",
        "codeLocation",
        "preExisting",
        "preExistingFixRationale"
    ]);
    schema
}

pub(super) fn discovery_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "candidates": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": discovery_finding_schema()
            },
            "assessment": assessment_schema(),
            "reviewContext": {
                "type": "array",
                "maxItems": MAX_CONTEXT_RANGES,
                "items": code_location_schema()
            },
            "externalReferences": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "reference": {"type": "string", "maxLength": MAX_PATH_STRING},
                        "explanation": {"type": "string", "maxLength": MAX_SHORT_STRING}
                    },
                    "required": ["reference", "explanation"]
                }
            }
        },
        "required": ["candidates", "assessment", "reviewContext", "externalReferences"]
    })
}

pub(super) fn verification_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "findings": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": verified_finding_schema()
            },
            "outOfScopeFindings": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": verified_finding_schema()
            },
            "unverifiedFindings": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": verified_finding_schema()
            },
            "assessment": assessment_schema()
        },
        "required": [
            "findings",
            "outOfScopeFindings",
            "unverifiedFindings",
            "assessment"
        ]
    })
}

pub(super) fn fix_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "classificationUpdates": {
                "type": "array",
                "maxItems": MAX_STAGE_ITEMS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "findingIndex": {"type": "integer", "minimum": 0},
                        "preExisting": {
                            "type": "string",
                            "enum": ["true", "false", "undetermined"]
                        },
                        "preExistingFixRationale": {
                            "type": ["string", "null"],
                            "maxLength": MAX_SHORT_STRING
                        },
                        "disposition": {
                            "type": "string",
                            "enum": ["fixed", "rejected", "unresolved"]
                        }
                    },
                    "required": [
                        "findingIndex",
                        "preExisting",
                        "preExistingFixRationale",
                        "disposition"
                    ]
                }
            },
            "resolution": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["complete", "partial", "failed"]
                    },
                    "fixedCount": {"type": "integer", "minimum": 0},
                    "rejectedCount": {"type": "integer", "minimum": 0},
                    "unresolvedCount": {"type": "integer", "minimum": 0},
                    "summary": {
                        "type": "array",
                        "maxItems": 5,
                        "items": {"type": "string", "maxLength": MAX_SHORT_STRING}
                    },
                    "tests": {
                        "type": "array",
                        "maxItems": MAX_STAGE_ITEMS,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "command": {"type": "string", "maxLength": MAX_PATH_STRING},
                                "status": {
                                    "type": "string",
                                    "enum": ["passed", "failed", "notRun"]
                                }
                            },
                            "required": ["command", "status"]
                        }
                    },
                    "commitSha": {"type": ["string", "null"], "maxLength": MAX_SHORT_STRING}
                },
                "required": [
                    "status",
                    "fixedCount",
                    "rejectedCount",
                    "unresolvedCount",
                    "summary",
                    "tests",
                    "commitSha"
                ]
            }
        },
        "required": ["classificationUpdates", "resolution"]
    })
}

pub(super) fn validate(value: &Value, schema: &Value) -> anyhow::Result<()> {
    validate_at(value, schema, "$")
}

fn validate_at(value: &Value, schema: &Value, path: &str) -> anyhow::Result<()> {
    if let Some(types) = schema.get("type") {
        let valid_type = match types {
            Value::String(expected) => value_matches_type(value, expected),
            Value::Array(expected) => expected
                .iter()
                .filter_map(Value::as_str)
                .any(|expected| value_matches_type(value, expected)),
            _ => false,
        };
        anyhow::ensure!(valid_type, "{path} has the wrong JSON type");
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        anyhow::ensure!(values.contains(value), "{path} is not an allowed value");
    }
    if let Some(object) = value.as_object() {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                anyhow::ensure!(object.contains_key(key), "{path}.{key} is required");
            }
        }
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            for key in object.keys() {
                anyhow::ensure!(properties.contains_key(key), "{path}.{key} is not allowed");
            }
        }
        for (key, item) in object {
            if let Some(property_schema) = properties.get(key) {
                validate_at(item, property_schema, &format!("{path}.{key}"))?;
            }
        }
        if let (Some(start), Some(end)) = (
            object.get("start").and_then(Value::as_u64),
            object.get("end").and_then(Value::as_u64),
        ) {
            anyhow::ensure!(end >= start, "{path}.end precedes {path}.start");
        }
    }
    if let Some(array) = value.as_array() {
        if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
            anyhow::ensure!(
                array.len() <= usize::try_from(max_items).unwrap_or(usize::MAX),
                "{path} has too many items"
            );
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_at(item, item_schema, &format!("{path}[{index}]"))?;
            }
        }
    }
    if let Some(text) = value.as_str()
        && let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64)
    {
        anyhow::ensure!(
            text.chars().count() <= usize::try_from(max_length).unwrap_or(usize::MAX),
            "{path} is too long"
        );
    }
    if let Some(number) = value.as_f64() {
        if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
            anyhow::ensure!(number >= minimum, "{path} is below its minimum");
        }
        if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
            anyhow::ensure!(number <= maximum, "{path} exceeds its maximum");
        }
    }
    Ok(())
}

fn value_matches_type(value: &Value, expected: &str) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
