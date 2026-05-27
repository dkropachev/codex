use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;

const VALID_JSON_SCHEMA_TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "null", "array", "object",
];

pub(crate) fn validate_json_against_schema(schema: &JsonValue, value: &JsonValue) -> Result<()> {
    validate_json_against_schema_at(schema, value, "$")
}

pub(crate) fn validate_json_schema_document(schema: &JsonValue, label: &str) -> Result<()> {
    validate_json_schema_at(schema, label)
}

fn validate_json_against_schema_at(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<()> {
    validate_combinators(schema, value, path)?;

    if let Some(enum_values) = schema.get("enum").and_then(JsonValue::as_array)
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: value must be one of {enum_values:?}"
        ));
    }

    if let Some(expected) = schema.get("const")
        && value != expected
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: value must equal {expected:?}"
        ));
    }

    let Some(type_spec) = schema.get("type") else {
        return validate_shape(schema, value, path);
    };

    match type_spec {
        JsonValue::String(type_name) => validate_typed_value(schema, value, path, type_name),
        JsonValue::Array(types) => {
            if types.iter().any(|ty| ty == "null") && value.is_null() {
                return Ok(());
            }

            for type_name in types.iter().filter_map(JsonValue::as_str) {
                if matches_json_type(type_name, value) {
                    return validate_typed_value(schema, value, path, type_name);
                }
            }

            Err(anyhow!(
                "workflow contract violation at {path}: value does not match any allowed type"
            ))
        }
        _ => Err(anyhow!(
            "workflow contract violation at {path}: invalid schema type"
        )),
    }
}

fn validate_combinators(schema: &JsonValue, value: &JsonValue, path: &str) -> Result<()> {
    if let Some(any_of) = schema.get("anyOf").and_then(JsonValue::as_array)
        && !any_of
            .iter()
            .any(|candidate| validate_json_against_schema_at(candidate, value, path).is_ok())
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: value does not match any allowed schema"
        ));
    }

    if let Some(one_of) = schema.get("oneOf").and_then(JsonValue::as_array) {
        let matches = one_of
            .iter()
            .filter(|candidate| validate_json_against_schema_at(candidate, value, path).is_ok())
            .count();
        if matches != 1 {
            return Err(anyhow!(
                "workflow contract violation at {path}: value must match exactly one allowed schema"
            ));
        }
    }

    if let Some(all_of) = schema.get("allOf").and_then(JsonValue::as_array) {
        for candidate in all_of {
            validate_json_against_schema_at(candidate, value, path)?;
        }
    }

    Ok(())
}

fn validate_shape(schema: &JsonValue, value: &JsonValue, path: &str) -> Result<()> {
    if schema.get("properties").is_some() || schema.get("additionalProperties").is_some() {
        return validate_object(schema, value, path);
    }
    if schema.get("prefixItems").is_some() || schema.get("items").is_some() {
        return validate_array(schema, value, path);
    }
    Ok(())
}

fn validate_typed_value(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
    type_name: &str,
) -> Result<()> {
    match type_name {
        "string" if value.is_string() => Ok(()),
        "number" if value.is_number() => Ok(()),
        "integer" if value.as_i64().is_some() || value.as_u64().is_some() => Ok(()),
        "boolean" if value.is_boolean() => Ok(()),
        "null" if value.is_null() => Ok(()),
        "array" => validate_array(schema, value, path),
        "object" => validate_object(schema, value, path),
        _ => Err(anyhow!(
            "workflow contract violation at {path}: expected {type_name}"
        )),
    }
}

fn validate_object(schema: &JsonValue, value: &JsonValue, path: &str) -> Result<()> {
    let Some(object) = value.as_object() else {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected object"
        ));
    };

    let properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default();

    for required_name in required.iter().filter_map(JsonValue::as_str) {
        if !object.contains_key(required_name) {
            return Err(anyhow!(
                "workflow contract violation at {path}.{required_name}: missing required property"
            ));
        }
    }

    let additional_properties = schema.get("additionalProperties");
    for (name, item_value) in object {
        if let Some(property_schema) = properties.get(name) {
            validate_json_against_schema_at(property_schema, item_value, &join_path(path, name))?;
            continue;
        }

        match additional_properties {
            Some(JsonValue::Bool(false)) | None => {
                return Err(anyhow!(
                    "workflow contract violation at {path}.{name}: unexpected property"
                ));
            }
            Some(JsonValue::Bool(true)) => continue,
            Some(schema) => {
                validate_json_against_schema_at(schema, item_value, &join_path(path, name))?
            }
        }
    }

    Ok(())
}

fn validate_array(schema: &JsonValue, value: &JsonValue, path: &str) -> Result<()> {
    let Some(array) = value.as_array() else {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected array"
        ));
    };

    if let Some(prefix_items) = schema.get("prefixItems").and_then(JsonValue::as_array) {
        let min_items = schema
            .get("minItems")
            .and_then(JsonValue::as_u64)
            .unwrap_or(prefix_items.len() as u64);
        let max_items = schema
            .get("maxItems")
            .and_then(JsonValue::as_u64)
            .unwrap_or(prefix_items.len() as u64);
        let array_len = array.len() as u64;
        if array_len < min_items || array_len > max_items {
            return Err(anyhow!(
                "workflow contract violation at {path}: expected between {min_items} and {max_items} items"
            ));
        }

        for (index, item_schema) in prefix_items.iter().enumerate() {
            if let Some(item_value) = array.get(index) {
                validate_json_against_schema_at(
                    item_schema,
                    item_value,
                    &format!("{path}[{index}]"),
                )?;
            }
        }

        if let Some(items_schema) = schema.get("items") {
            for (index, item_value) in array.iter().enumerate().skip(prefix_items.len()) {
                validate_json_against_schema_at(
                    items_schema,
                    item_value,
                    &format!("{path}[{index}]"),
                )?;
            }
        }

        return Ok(());
    }

    if let Some(items_schema) = schema.get("items") {
        for (index, item_value) in array.iter().enumerate() {
            validate_json_against_schema_at(items_schema, item_value, &format!("{path}[{index}]"))?;
        }
    }

    let array_len = array.len() as u64;
    if let Some(min_items) = schema.get("minItems").and_then(JsonValue::as_u64)
        && array_len < min_items
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected at least {min_items} items"
        ));
    }
    if let Some(max_items) = schema.get("maxItems").and_then(JsonValue::as_u64)
        && array_len > max_items
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected at most {max_items} items"
        ));
    }

    Ok(())
}

fn matches_json_type(type_name: &str, value: &JsonValue) -> bool {
    match type_name {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn join_path(path: &str, segment: &str) -> String {
    if segment
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
    {
        format!("{path}.{segment}")
    } else {
        format!("{path}[{segment:?}]")
    }
}

fn validate_json_schema_at(schema: &JsonValue, path: &str) -> Result<()> {
    let Some(object) = schema.as_object() else {
        return Err(anyhow!("{path} must be a JSON schema object"));
    };

    if let Some(type_spec) = object.get("type") {
        validate_schema_type(type_spec, &format!("{path}.type"))?;
    }
    if let Some(enum_values) = object.get("enum")
        && !enum_values.is_array()
    {
        return Err(anyhow!("{path}.enum must be an array"));
    }
    if let Some(properties) = object.get("properties") {
        let Some(properties) = properties.as_object() else {
            return Err(anyhow!("{path}.properties must be an object"));
        };
        for (name, property_schema) in properties {
            validate_json_schema_at(property_schema, &format!("{path}.properties.{name}"))?;
        }
    }
    if let Some(required) = object.get("required") {
        let Some(required) = required.as_array() else {
            return Err(anyhow!("{path}.required must be an array"));
        };
        for (index, required_name) in required.iter().enumerate() {
            if !required_name.is_string() {
                return Err(anyhow!("{path}.required[{index}] must be a string"));
            }
        }
    }
    if let Some(additional_properties) = object.get("additionalProperties") {
        match additional_properties {
            JsonValue::Bool(_) => {}
            schema => validate_json_schema_at(schema, &format!("{path}.additionalProperties"))?,
        }
    }
    if let Some(items) = object.get("items") {
        validate_json_schema_at(items, &format!("{path}.items"))?;
    }
    if let Some(prefix_items) = object.get("prefixItems") {
        let Some(prefix_items) = prefix_items.as_array() else {
            return Err(anyhow!("{path}.prefixItems must be an array"));
        };
        for (index, item_schema) in prefix_items.iter().enumerate() {
            validate_json_schema_at(item_schema, &format!("{path}.prefixItems[{index}]"))?;
        }
    }
    for union_key in ["anyOf", "oneOf", "allOf"] {
        let Some(entries) = object.get(union_key) else {
            continue;
        };
        let Some(entries) = entries.as_array() else {
            return Err(anyhow!("{path}.{union_key} must be an array"));
        };
        if entries.is_empty() {
            return Err(anyhow!("{path}.{union_key} must not be empty"));
        }
        for (index, entry) in entries.iter().enumerate() {
            validate_json_schema_at(entry, &format!("{path}.{union_key}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_schema_type(type_spec: &JsonValue, path: &str) -> Result<()> {
    match type_spec {
        JsonValue::String(type_name) if VALID_JSON_SCHEMA_TYPES.contains(&type_name.as_str()) => {
            Ok(())
        }
        JsonValue::String(type_name) => {
            Err(anyhow!("{path} contains unsupported type `{type_name}`"))
        }
        JsonValue::Array(types) if !types.is_empty() => {
            for (index, type_name) in types.iter().enumerate() {
                let Some(type_name) = type_name.as_str() else {
                    return Err(anyhow!("{path}[{index}] must be a string"));
                };
                if !VALID_JSON_SCHEMA_TYPES.contains(&type_name) {
                    return Err(anyhow!(
                        "{path}[{index}] contains unsupported type `{type_name}`"
                    ));
                }
            }
            Ok(())
        }
        JsonValue::Array(_) => Err(anyhow!("{path} must not be empty")),
        _ => Err(anyhow!("{path} must be a string or array of strings")),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::validate_json_against_schema;
    use super::validate_json_schema_document;

    #[test]
    fn validate_json_against_schema_supports_one_of_and_const() {
        let schema = json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "a" } }, "required": ["kind"] },
                { "type": "object", "properties": { "kind": { "const": "b" } }, "required": ["kind"] }
            ]
        });

        validate_json_against_schema(&schema, &json!({ "kind": "a" }))
            .expect("oneOf schema should match");
        let err = validate_json_against_schema(&schema, &json!({ "kind": "c" }))
            .expect_err("unknown const should fail");

        assert_eq!(
            err.to_string(),
            "workflow contract violation at $: value must match exactly one allowed schema"
        );
    }

    #[test]
    fn validate_json_schema_document_rejects_malformed_schema() {
        let err =
            validate_json_schema_document(&json!({ "type": ["object", 1] }), "api.outputSchema")
                .expect_err("schema type arrays must contain only strings");

        assert_eq!(err.to_string(), "api.outputSchema.type[1] must be a string");
    }
}
