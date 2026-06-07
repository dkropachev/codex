use std::collections::BTreeSet;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::spec::WorkflowSpec;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandOptionHint {
    pub display: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCommandCompletionSuggestion {
    pub display: String,
    pub insert_text: String,
    pub description: Option<String>,
}

pub(crate) fn command_option_hints_from_spec(
    _spec: &WorkflowSpec,
) -> Vec<WorkflowCommandOptionHint> {
    Vec::new()
}

pub(crate) fn command_option_hints_from_input_schema(
    schema: Option<&JsonValue>,
) -> Vec<WorkflowCommandOptionHint> {
    input_schema_option_hints(schema)
}

fn input_schema_option_hints(schema: Option<&JsonValue>) -> Vec<WorkflowCommandOptionHint> {
    let Some(schema) = schema else {
        return Vec::new();
    };
    let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) else {
        return Vec::new();
    };

    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .map(ToString::to_string)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut names = properties.keys().cloned().collect::<Vec<_>>();
    names.sort_by(|left, right| {
        required
            .contains(left)
            .cmp(&required.contains(right))
            .reverse()
            .then_with(|| {
                crate::input_adapter::workflow_flag_name(left)
                    .cmp(&crate::input_adapter::workflow_flag_name(right))
            })
    });

    names
        .into_iter()
        .filter_map(|name| {
            let property = properties.get(&name)?;
            let flag = crate::input_adapter::workflow_flag_name(&name);
            let value_hint = json_schema_value_hint(property);
            let display = match value_hint {
                Some(value_hint) => format!("{flag} {value_hint}"),
                None => flag,
            };
            let description = property
                .get("description")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            let description = if required.contains(&name) {
                Some(match description {
                    Some(description) => format!("required · {description}"),
                    None => "required".to_string(),
                })
            } else {
                description
            };
            Some(WorkflowCommandOptionHint {
                display,
                description,
            })
        })
        .collect()
}

pub(crate) fn json_schema_value_hint(schema: &JsonValue) -> Option<String> {
    if let Some(enum_values) = schema.get("enum").and_then(JsonValue::as_array) {
        let values = enum_values
            .iter()
            .filter_map(schema_enum_value_text)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return Some(format!("<{}>", values.join("|")));
        }
    }

    match schema_type_name(schema)?.as_str() {
        "boolean" => None,
        "string" => Some("<string>".to_string()),
        "integer" => Some("<integer>".to_string()),
        "number" => Some("<number>".to_string()),
        "array" => match schema.get("items").and_then(schema_type_name).as_deref() {
            Some("string") => Some("<string[]>".to_string()),
            _ => Some("<json>".to_string()),
        },
        "object" => Some("<json>".to_string()),
        _ => Some("<value>".to_string()),
    }
}

pub(crate) fn schema_type_name(schema: &JsonValue) -> Option<String> {
    if let Some(type_name) = schema.get("type").and_then(JsonValue::as_str) {
        return Some(type_name.to_string());
    }

    let types = schema.get("type")?.as_array()?;
    types
        .iter()
        .filter_map(JsonValue::as_str)
        .find(|type_name| *type_name != "null")
        .map(ToString::to_string)
}

fn schema_enum_value_text(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) if !text.is_empty() => Some(text.clone()),
        JsonValue::Number(number) => Some(number.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::WorkflowCommandOptionHint;
    use super::command_option_hints_from_input_schema;
    use super::command_option_hints_from_spec;
    use crate::spec::WorkflowSpec;

    #[test]
    fn workflow_yaml_usage_options_are_ignored_for_schema_hints() {
        let spec = WorkflowSpec {
            usage: json!({
                "options": [
                    "--workflow-id <id>",
                    {
                        "flag": "--format",
                        "valueHint": "<summary|full>",
                        "description": "Output format"
                    }
                ]
            }),
            ..WorkflowSpec::default()
        };

        assert_eq!(command_option_hints_from_spec(&spec), Vec::new());
    }

    #[test]
    fn input_schema_properties_become_flag_hints() {
        let spec = WorkflowSpec {
            api: json!({
                "inputSchema": {
                    "type": "object",
                    "required": ["workflowId"],
                    "properties": {
                        "workflowId": {
                            "type": "string",
                            "description": "Workflow identifier"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["summary", "full"],
                            "description": "Output format"
                        },
                        "includeComments": {
                            "type": "boolean",
                            "description": "Include comment bodies"
                        }
                    }
                }
            }),
            ..WorkflowSpec::default()
        };

        assert_eq!(
            command_option_hints_from_input_schema(spec.api.get("inputSchema")),
            vec![
                WorkflowCommandOptionHint {
                    display: "--workflow-id <string>".to_string(),
                    description: Some("required · Workflow identifier".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--format <summary|full>".to_string(),
                    description: Some("Output format".to_string()),
                },
                WorkflowCommandOptionHint {
                    display: "--include-comments".to_string(),
                    description: Some("Include comment bodies".to_string()),
                },
            ]
        );
    }
}
