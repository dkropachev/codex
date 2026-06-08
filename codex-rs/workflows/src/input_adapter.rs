use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map as JsonMap;
use serde_json::Number as JsonNumber;
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum WorkflowCompletionMode {
    #[default]
    Field,
    Value,
}

/// Structured completion request passed to a workflow `complete(ctx, request)`
/// hook. The hook receives partially normalized input and the active field
/// context instead of legacy raw argv/text command data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCompletionRequest {
    #[serde(default = "empty_object")]
    pub input: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_field: Option<String>,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub mode: WorkflowCompletionMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub replacement_prefix: String,
}

pub type WorkflowCommandInput = WorkflowCompletionRequest;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowInputAdapterError {
    #[error("workflow input must be valid JSON: {0}")]
    InvalidJson(String),
    #[error("workflow input must be a JSON object when merging input flags")]
    NonObjectInput,
    #[error("unknown workflow input field '{0}'")]
    UnknownField(String),
    #[error("workflow input field '{field}' requires {expected}, got '{value}'")]
    InvalidFieldValue {
        field: String,
        expected: &'static str,
        value: String,
    },
    #[error("workflow input field '{field}' must be one of {allowed}")]
    InvalidEnumValue { field: String, allowed: String },
}

pub fn normalize_workflow_input_json(
    raw_input: Option<&str>,
    input_fields: BTreeMap<String, String>,
    input_schema: Option<&JsonValue>,
) -> Result<JsonValue, WorkflowInputAdapterError> {
    let raw_input = raw_input.unwrap_or("{}");
    let mut value = serde_json::from_str::<JsonValue>(raw_input)
        .map_err(|err| WorkflowInputAdapterError::InvalidJson(err.to_string()))?;
    if !input_fields.is_empty() {
        merge_input_fields(&mut value, input_fields, input_schema)?;
    }
    if let Some(input_schema) = input_schema {
        crate::workflow_contract_validation::validate_json_against_schema(input_schema, &value)
            .map_err(|err| WorkflowInputAdapterError::InvalidJson(err.to_string()))?;
    }
    Ok(value)
}

pub(crate) fn normalize_workflow_input_string(
    raw_input: Option<&str>,
    input_fields: BTreeMap<String, String>,
    input_schema: Option<&JsonValue>,
) -> Result<String, WorkflowInputAdapterError> {
    let value = normalize_workflow_input_json(raw_input, input_fields, input_schema)?;
    serde_json::to_string(&value)
        .map_err(|err| WorkflowInputAdapterError::InvalidJson(err.to_string()))
}

pub fn workflow_completion_request_from_text(
    text: &str,
    input_schema: Option<&JsonValue>,
) -> WorkflowCompletionRequest {
    let text = text.trim_start();
    let argv = if text.is_empty() {
        Vec::new()
    } else {
        shlex::split(text)
            .unwrap_or_else(|| text.split_whitespace().map(ToString::to_string).collect())
    };
    workflow_completion_request_from_argv(text, &argv, input_schema)
}

pub(crate) fn workflow_flag_name(field_name: &str) -> String {
    let mut flag = String::from("--");
    for (idx, ch) in field_name.chars().enumerate() {
        match ch {
            '_' => flag.push('-'),
            ch if ch.is_ascii_uppercase() => {
                if idx > 0 {
                    flag.push('-');
                }
                flag.push(ch.to_ascii_lowercase());
            }
            ch => flag.push(ch),
        }
    }
    flag
}

pub(crate) fn normalize_input_field_name(flag: &str) -> String {
    let mut name = String::with_capacity(flag.len());
    let mut uppercase_next = false;
    for ch in flag.chars() {
        if ch == '-' {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            name.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            name.push(ch);
        }
    }
    name
}

pub fn completion_suggestions_from_schema(
    request: &WorkflowCompletionRequest,
    schema: Option<&JsonValue>,
) -> Vec<crate::command_completion::WorkflowCommandCompletionSuggestion> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    match request.mode {
        WorkflowCompletionMode::Field => field_completion_suggestions(request, schema),
        WorkflowCompletionMode::Value => value_completion_suggestions(request, schema),
    }
}

fn workflow_completion_request_from_argv(
    text: &str,
    argv: &[String],
    input_schema: Option<&JsonValue>,
) -> WorkflowCompletionRequest {
    let mut fields = BTreeMap::new();
    let trailing_space = text.chars().next_back().is_some_and(char::is_whitespace);
    let mut active_field = None;
    let mut prefix = String::new();
    let mut mode = WorkflowCompletionMode::Field;
    let mut index = 0;

    while index < argv.len() {
        let arg = &argv[index];
        let Some(flag) = arg.strip_prefix("--").filter(|flag| !flag.is_empty()) else {
            index += 1;
            continue;
        };

        if let Some((name, value)) = flag.split_once('=') {
            let field = normalize_input_field_name(name);
            if index + 1 == argv.len() && !trailing_space {
                active_field = Some(field);
                prefix = value.to_string();
                mode = WorkflowCompletionMode::Value;
            } else {
                fields.insert(field, value.to_string());
            }
            index += 1;
            continue;
        }

        let field = normalize_input_field_name(flag);
        match argv.get(index + 1) {
            Some(next) if !next.starts_with("--") => {
                if index + 2 == argv.len() && !trailing_space {
                    active_field = Some(field);
                    prefix = next.to_string();
                    mode = WorkflowCompletionMode::Value;
                } else {
                    fields.insert(field, next.to_string());
                }
                index += 2;
            }
            _ if index + 1 == argv.len() => {
                if trailing_space && field_expects_value(input_schema, &field) {
                    active_field = Some(field);
                    prefix.clear();
                    mode = WorkflowCompletionMode::Value;
                } else if trailing_space {
                    fields.insert(field, "true".to_string());
                } else {
                    prefix = arg.clone();
                    mode = WorkflowCompletionMode::Field;
                }
                index += 1;
            }
            _ => {
                fields.insert(field, "true".to_string());
                index += 1;
            }
        }
    }

    let input = input_fields_to_value(fields, input_schema);
    WorkflowCompletionRequest {
        input,
        active_field,
        prefix: prefix_for_mode(prefix, mode),
        mode,
        replacement_prefix: if trailing_space {
            text.to_string()
        } else {
            replacement_prefix(text)
        },
    }
}

fn prefix_for_mode(prefix: String, mode: WorkflowCompletionMode) -> String {
    match mode {
        WorkflowCompletionMode::Field => prefix.strip_prefix("--").unwrap_or(&prefix).to_string(),
        WorkflowCompletionMode::Value => prefix,
    }
}

fn replacement_prefix(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let Some(last_non_ws) = text
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_whitespace())
    else {
        return text.to_string();
    };
    let token_end = last_non_ws.0 + last_non_ws.1.len_utf8();
    let before_trimmed = &text[..token_end];
    let token_start = before_trimmed
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    if before_trimmed[token_start..].starts_with("--")
        && before_trimmed[token_start..].contains('=')
    {
        let token = &before_trimmed[token_start..];
        let value_start = token
            .find('=')
            .map(|index| token_start + index + 1)
            .unwrap_or(token_end);
        return before_trimmed[..value_start].to_string();
    }
    before_trimmed[..token_start].to_string()
}

fn input_fields_to_value(
    input_fields: BTreeMap<String, String>,
    input_schema: Option<&JsonValue>,
) -> JsonValue {
    let mut value = JsonValue::Object(JsonMap::new());
    let _ = merge_input_fields(&mut value, input_fields, input_schema);
    value
}

fn merge_input_fields(
    value: &mut JsonValue,
    input_fields: BTreeMap<String, String>,
    input_schema: Option<&JsonValue>,
) -> Result<(), WorkflowInputAdapterError> {
    let Some(object) = value.as_object_mut() else {
        return Err(WorkflowInputAdapterError::NonObjectInput);
    };
    for (key, raw_value) in input_fields {
        let property_schema = input_schema.and_then(|schema| property_schema(schema, &key));
        if input_schema.is_some()
            && property_schema.is_none()
            && schema_rejects_additional_properties(input_schema)
        {
            return Err(WorkflowInputAdapterError::UnknownField(key));
        }
        object.insert(
            key.clone(),
            parse_input_field_value(&key, &raw_value, property_schema)?,
        );
    }
    Ok(())
}

fn parse_input_field_value(
    field: &str,
    raw_value: &str,
    schema: Option<&JsonValue>,
) -> Result<JsonValue, WorkflowInputAdapterError> {
    let Some(schema) = schema else {
        return Ok(parse_json_or_string(raw_value));
    };
    let value = match schema_type_name(schema).as_deref() {
        Some("boolean") => parse_bool(field, raw_value)?,
        Some("integer") => parse_integer(field, raw_value)?,
        Some("number") => parse_number(field, raw_value)?,
        Some("array") => parse_array(field, raw_value, schema)?,
        Some("object") => parse_object(field, raw_value)?,
        Some("string") | None => JsonValue::String(raw_value.to_string()),
        Some(_) => parse_json_or_string(raw_value),
    };
    validate_enum_value(field, schema, &value)?;
    Ok(value)
}

fn parse_json_or_string(raw_value: &str) -> JsonValue {
    serde_json::from_str(raw_value).unwrap_or_else(|_| JsonValue::String(raw_value.to_string()))
}

fn parse_bool(field: &str, raw_value: &str) -> Result<JsonValue, WorkflowInputAdapterError> {
    match raw_value {
        "true" => Ok(JsonValue::Bool(true)),
        "false" => Ok(JsonValue::Bool(false)),
        _ => Err(WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "a boolean",
            value: raw_value.to_string(),
        }),
    }
}

fn parse_integer(field: &str, raw_value: &str) -> Result<JsonValue, WorkflowInputAdapterError> {
    raw_value.parse::<i64>().map(JsonValue::from).map_err(|_| {
        WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "an integer",
            value: raw_value.to_string(),
        }
    })
}

fn parse_number(field: &str, raw_value: &str) -> Result<JsonValue, WorkflowInputAdapterError> {
    let number = raw_value
        .parse::<f64>()
        .ok()
        .and_then(JsonNumber::from_f64)
        .ok_or_else(|| WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "a number",
            value: raw_value.to_string(),
        })?;
    Ok(JsonValue::Number(number))
}

fn parse_array(
    field: &str,
    raw_value: &str,
    schema: &JsonValue,
) -> Result<JsonValue, WorkflowInputAdapterError> {
    if raw_value.trim_start().starts_with('[') {
        let value = serde_json::from_str::<JsonValue>(raw_value).map_err(|_| {
            WorkflowInputAdapterError::InvalidFieldValue {
                field: field.to_string(),
                expected: "a JSON array",
                value: raw_value.to_string(),
            }
        })?;
        if value.is_array() {
            return Ok(value);
        }
        return Err(WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "a JSON array",
            value: raw_value.to_string(),
        });
    }

    let item_schema = schema.get("items");
    if item_schema.and_then(schema_type_name).as_deref() == Some("string") {
        return Ok(JsonValue::Array(
            raw_value
                .split(',')
                .filter(|value| !value.is_empty())
                .map(|value| JsonValue::String(value.to_string()))
                .collect(),
        ));
    }

    Err(WorkflowInputAdapterError::InvalidFieldValue {
        field: field.to_string(),
        expected: "a JSON array",
        value: raw_value.to_string(),
    })
}

fn parse_object(field: &str, raw_value: &str) -> Result<JsonValue, WorkflowInputAdapterError> {
    let value = serde_json::from_str::<JsonValue>(raw_value).map_err(|_| {
        WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "a JSON object",
            value: raw_value.to_string(),
        }
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(WorkflowInputAdapterError::InvalidFieldValue {
            field: field.to_string(),
            expected: "a JSON object",
            value: raw_value.to_string(),
        })
    }
}

fn validate_enum_value(
    field: &str,
    schema: &JsonValue,
    value: &JsonValue,
) -> Result<(), WorkflowInputAdapterError> {
    let Some(enum_values) = schema.get("enum").and_then(JsonValue::as_array) else {
        return Ok(());
    };
    if enum_values.iter().any(|candidate| candidate == value) {
        return Ok(());
    }
    let allowed = enum_values
        .iter()
        .map(|value| match value {
            JsonValue::String(value) => value.clone(),
            value => value.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(WorkflowInputAdapterError::InvalidEnumValue {
        field: field.to_string(),
        allowed,
    })
}

fn field_completion_suggestions(
    request: &WorkflowCompletionRequest,
    schema: &JsonValue,
) -> Vec<crate::command_completion::WorkflowCommandCompletionSuggestion> {
    let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) else {
        return Vec::new();
    };
    let mut names = properties.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .filter(|name| name.starts_with(&request.prefix))
        .filter_map(|name| {
            let property = properties.get(&name)?;
            let flag = workflow_flag_name(&name);
            let value_hint = crate::command_completion::json_schema_value_hint(property);
            let insert_text = if value_hint.is_some() {
                format!("{}{} ", request.replacement_prefix, flag)
            } else {
                format!("{}{}", request.replacement_prefix, flag)
            };
            Some(
                crate::command_completion::WorkflowCommandCompletionSuggestion {
                    display: value_hint
                        .map(|value_hint| format!("{flag} {value_hint}"))
                        .unwrap_or_else(|| flag.clone()),
                    insert_text,
                    description: property
                        .get("description")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string),
                },
            )
        })
        .collect()
}

fn value_completion_suggestions(
    request: &WorkflowCompletionRequest,
    schema: &JsonValue,
) -> Vec<crate::command_completion::WorkflowCommandCompletionSuggestion> {
    let Some(active_field) = request.active_field.as_deref() else {
        return Vec::new();
    };
    let Some(property) = property_schema(schema, active_field) else {
        return Vec::new();
    };
    let enum_values = property
        .get("enum")
        .and_then(JsonValue::as_array)
        .or_else(|| {
            if schema_type_name(property).as_deref() == Some("array") {
                property
                    .get("items")
                    .and_then(|items| items.get("enum"))
                    .and_then(JsonValue::as_array)
            } else {
                None
            }
        });
    let Some(enum_values) = enum_values else {
        return Vec::new();
    };
    enum_values
        .iter()
        .filter_map(enum_completion_text)
        .filter(|value| value.starts_with(&request.prefix))
        .map(
            |value| crate::command_completion::WorkflowCommandCompletionSuggestion {
                display: value.clone(),
                insert_text: format!("{}{}", request.replacement_prefix, value),
                description: None,
            },
        )
        .collect()
}

fn enum_completion_text(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Number(value) => Some(value.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn property_schema<'a>(schema: &'a JsonValue, field: &str) -> Option<&'a JsonValue> {
    schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get(field))
}

pub(crate) fn field_expects_value(input_schema: Option<&JsonValue>, field: &str) -> bool {
    let Some(schema) = input_schema else {
        return true;
    };
    property_schema(schema, field)
        .and_then(schema_type_name)
        .as_deref()
        != Some("boolean")
}

fn schema_rejects_additional_properties(schema: Option<&JsonValue>) -> bool {
    schema
        .and_then(|schema| schema.get("additionalProperties"))
        .is_none_or(|value| value == false)
}

fn schema_type_name(schema: &JsonValue) -> Option<String> {
    if let Some(type_name) = schema.get("type").and_then(JsonValue::as_str) {
        return Some(type_name.to_string());
    }
    schema
        .get("type")?
        .as_array()?
        .iter()
        .filter_map(JsonValue::as_str)
        .find(|type_name| *type_name != "null")
        .map(ToString::to_string)
}

fn empty_object() -> JsonValue {
    JsonValue::Object(JsonMap::new())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_schema_typed_input_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "reviewId": { "type": "string" },
                "includeComments": { "type": "boolean" },
                "maxFindings": { "type": "integer" },
                "confidence": { "type": "number" },
                "allowedAreas": { "type": "array", "items": { "type": "string" } },
                "format": { "type": "string", "enum": ["json", "markdown"] }
            },
            "additionalProperties": false
        });

        let input = normalize_workflow_input_json(
            Some(r#"{"reviewId":"old","format":"json"}"#),
            BTreeMap::from([
                ("reviewId".to_string(), "r2".to_string()),
                ("includeComments".to_string(), "true".to_string()),
                ("maxFindings".to_string(), "3".to_string()),
                ("confidence".to_string(), "0.75".to_string()),
                ("allowedAreas".to_string(), "Test,Code".to_string()),
                ("format".to_string(), "markdown".to_string()),
            ]),
            Some(&schema),
        )
        .unwrap();

        assert_eq!(
            input,
            json!({
                "reviewId": "r2",
                "includeComments": true,
                "maxFindings": 3,
                "confidence": 0.75,
                "allowedAreas": ["Test", "Code"],
                "format": "markdown",
            })
        );
    }

    #[test]
    fn normalizes_nullable_schema_typed_array_input_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "allowedAreas": {
                    "type": ["array", "null"],
                    "items": { "type": "string" }
                }
            },
            "additionalProperties": false
        });

        let input = normalize_workflow_input_json(
            Some("{}"),
            BTreeMap::from([("allowedAreas".to_string(), "Test,Code".to_string())]),
            Some(&schema),
        )
        .unwrap();

        assert_eq!(
            input,
            json!({
                "allowedAreas": ["Test", "Code"],
            })
        );
    }

    #[test]
    fn rejects_unknown_schema_field() {
        let schema = json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        });

        let err = normalize_workflow_input_json(
            Some("{}"),
            BTreeMap::from([("extra".to_string(), "value".to_string())]),
            Some(&schema),
        )
        .unwrap_err();

        assert_eq!(
            err,
            WorkflowInputAdapterError::UnknownField("extra".to_string())
        );
    }

    #[test]
    fn completion_request_uses_structured_context() {
        let schema = json!({
            "type": "object",
            "properties": {
                "reviewId": { "type": "string" },
                "format": { "type": "string", "enum": ["json", "markdown"] }
            },
            "additionalProperties": false
        });

        let request =
            workflow_completion_request_from_text("--review-id 123 --format m", Some(&schema));

        assert_eq!(
            request,
            WorkflowCompletionRequest {
                input: json!({ "reviewId": "123" }),
                active_field: Some("format".to_string()),
                prefix: "m".to_string(),
                mode: WorkflowCompletionMode::Value,
                replacement_prefix: "--review-id 123 --format ".to_string(),
            }
        );
    }

    #[test]
    fn completion_request_treats_trailing_value_flag_as_active_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "reviewId": { "type": "string" },
                "archive": { "type": "boolean" }
            },
            "additionalProperties": false
        });

        let request = workflow_completion_request_from_text("--review-id ", Some(&schema));

        assert_eq!(
            request,
            WorkflowCompletionRequest {
                input: json!({}),
                active_field: Some("reviewId".to_string()),
                prefix: String::new(),
                mode: WorkflowCompletionMode::Value,
                replacement_prefix: "--review-id ".to_string(),
            }
        );
    }

    #[test]
    fn completion_request_treats_trailing_nullable_array_flag_as_active_field() {
        let schema = json!({
            "type": "object",
            "properties": {
                "allowedAreas": {
                    "type": ["array", "null"],
                    "items": {
                        "type": "string",
                        "enum": ["Test", "Code"]
                    }
                },
                "includeSkippedByLimit": { "type": "boolean" }
            },
            "additionalProperties": false
        });

        let request = workflow_completion_request_from_text("--allowed-areas ", Some(&schema));

        assert_eq!(
            request,
            WorkflowCompletionRequest {
                input: json!({}),
                active_field: Some("allowedAreas".to_string()),
                prefix: String::new(),
                mode: WorkflowCompletionMode::Value,
                replacement_prefix: "--allowed-areas ".to_string(),
            }
        );
    }

    #[test]
    fn completion_request_without_schema_treats_trailing_flag_as_value_field() {
        let request =
            workflow_completion_request_from_text("--review-id ", /*input_schema*/ None);

        assert_eq!(
            request,
            WorkflowCompletionRequest {
                input: json!({}),
                active_field: Some("reviewId".to_string()),
                prefix: String::new(),
                mode: WorkflowCompletionMode::Value,
                replacement_prefix: "--review-id ".to_string(),
            }
        );
    }

    #[test]
    fn completion_request_treats_trailing_boolean_flag_as_completed_input() {
        let schema = json!({
            "type": "object",
            "properties": {
                "reviewId": { "type": "string" },
                "archive": { "type": "boolean" }
            },
            "additionalProperties": false
        });

        let request = workflow_completion_request_from_text("--archive ", Some(&schema));

        assert_eq!(
            request,
            WorkflowCompletionRequest {
                input: json!({ "archive": true }),
                active_field: None,
                prefix: String::new(),
                mode: WorkflowCompletionMode::Field,
                replacement_prefix: "--archive ".to_string(),
            }
        );
    }

    #[test]
    fn schema_enum_completion_uses_replacement_prefix() {
        let schema = json!({
            "type": "object",
            "properties": {
                "format": { "type": "string", "enum": ["json", "markdown"] }
            }
        });
        let request = WorkflowCompletionRequest {
            input: json!({}),
            active_field: Some("format".to_string()),
            prefix: "m".to_string(),
            mode: WorkflowCompletionMode::Value,
            replacement_prefix: "--format ".to_string(),
        };

        assert_eq!(
            completion_suggestions_from_schema(&request, Some(&schema)),
            vec![
                crate::command_completion::WorkflowCommandCompletionSuggestion {
                    display: "markdown".to_string(),
                    insert_text: "--format markdown".to_string(),
                    description: None,
                }
            ]
        );
    }

    #[test]
    fn schema_array_item_enum_completion_uses_replacement_prefix() {
        let schema = json!({
            "type": "object",
            "properties": {
                "allowedAreas": {
                    "type": ["array", "null"],
                    "items": {
                        "type": "string",
                        "enum": ["Test", "Code"]
                    }
                }
            }
        });
        let request = WorkflowCompletionRequest {
            input: json!({}),
            active_field: Some("allowedAreas".to_string()),
            prefix: "C".to_string(),
            mode: WorkflowCompletionMode::Value,
            replacement_prefix: "--allowed-areas ".to_string(),
        };

        assert_eq!(
            completion_suggestions_from_schema(&request, Some(&schema)),
            vec![
                crate::command_completion::WorkflowCommandCompletionSuggestion {
                    display: "Code".to_string(),
                    insert_text: "--allowed-areas Code".to_string(),
                    description: None,
                }
            ]
        );
    }
}
