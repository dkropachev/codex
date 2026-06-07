use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;

pub(crate) fn validate_json_against_schema(schema: &JsonValue, value: &JsonValue) -> Result<()> {
    validate_json_against_schema_at(schema, value, "$")
}

fn validate_json_against_schema_at(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<()> {
    if let Some(any_of) = schema.get("anyOf").and_then(JsonValue::as_array) {
        if any_of
            .iter()
            .any(|candidate| validate_json_against_schema_at(candidate, value, path).is_ok())
        {
            return Ok(());
        }
        return Err(anyhow!(
            "workflow contract violation at {path}: value does not match any allowed schema"
        ));
    }

    if let Some(enum_values) = schema.get("enum").and_then(JsonValue::as_array)
        && !value.is_null()
        && !enum_values.iter().any(|candidate| candidate == value)
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: value must be one of {enum_values:?}"
        ));
    }

    let Some(type_spec) = schema.get("type") else {
        return validate_shape(schema, value, path);
    };

    match type_spec {
        JsonValue::String(type_name) => validate_typed_value(schema, value, path, type_name),
        JsonValue::Array(types) => {
            if types.iter().any(|ty| ty.as_str() == Some("null")) && value.is_null() {
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
        "number" if value.is_number() => validate_number_bounds(schema, value, path),
        "integer" if value.as_i64().is_some() || value.as_u64().is_some() => {
            validate_number_bounds(schema, value, path)
        }
        "boolean" if value.is_boolean() => Ok(()),
        "null" if value.is_null() => Ok(()),
        "array" => validate_array(schema, value, path),
        "object" => validate_object(schema, value, path),
        _ => Err(anyhow!(
            "workflow contract violation at {path}: expected {type_name}"
        )),
    }
}

fn validate_number_bounds(schema: &JsonValue, value: &JsonValue, path: &str) -> Result<()> {
    let Some(number) = value.as_f64() else {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected finite number"
        ));
    };

    if let Some(minimum) = schema.get("minimum").and_then(JsonValue::as_f64)
        && number < minimum
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected number >= {minimum}"
        ));
    }

    if let Some(maximum) = schema.get("maximum").and_then(JsonValue::as_f64)
        && number > maximum
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected number <= {maximum}"
        ));
    }

    Ok(())
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

    let min_items = schema.get("minItems").and_then(JsonValue::as_u64);
    let max_items = schema.get("maxItems").and_then(JsonValue::as_u64);
    if let Some(min_items) = min_items
        && (array.len() as u64) < min_items
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected at least {min_items} items"
        ));
    }
    if let Some(max_items) = max_items
        && (array.len() as u64) > max_items
    {
        return Err(anyhow!(
            "workflow contract violation at {path}: expected at most {max_items} items"
        ));
    }

    if let Some(prefix_items) = schema.get("prefixItems").and_then(JsonValue::as_array) {
        let min_items = min_items.unwrap_or(prefix_items.len() as u64);
        let max_items = max_items.unwrap_or(prefix_items.len() as u64);
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::validate_json_against_schema;

    #[test]
    fn validates_number_and_integer_bounds() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": { "type": ["integer", "null"], "minimum": 0 },
                "score": { "type": "number", "maximum": 10 }
            },
            "additionalProperties": false
        });

        validate_json_against_schema(&schema, &json!({ "limit": 0, "score": 10 }))
            .expect("bounds should accept edge values");
        validate_json_against_schema(&schema, &json!({ "limit": null, "score": 5 }))
            .expect("nullable integer should accept null");

        let low = validate_json_against_schema(&schema, &json!({ "limit": -1, "score": 5 }))
            .expect_err("minimum should reject low integer");
        assert_eq!(
            low.to_string(),
            "workflow contract violation at $.limit: expected number >= 0"
        );

        let high = validate_json_against_schema(&schema, &json!({ "limit": 1, "score": 11 }))
            .expect_err("maximum should reject high number");
        assert_eq!(
            high.to_string(),
            "workflow contract violation at $.score: expected number <= 10"
        );
    }

    #[test]
    fn validates_homogeneous_array_length_constraints() {
        let schema = json!({
            "type": "array",
            "items": { "type": "string" },
            "minItems": 1,
            "maxItems": 2
        });

        validate_json_against_schema(&schema, &json!(["Code"])).expect("one item is valid");
        validate_json_against_schema(&schema, &json!(["Code", "Test"]))
            .expect("two items are valid");

        let empty = validate_json_against_schema(&schema, &json!([]))
            .expect_err("minItems should reject empty array");
        assert_eq!(
            empty.to_string(),
            "workflow contract violation at $: expected at least 1 items"
        );

        let too_many = validate_json_against_schema(&schema, &json!(["Code", "Test", "Docs"]))
            .expect_err("maxItems should reject long array");
        assert_eq!(
            too_many.to_string(),
            "workflow contract violation at $: expected at most 2 items"
        );
    }
}
