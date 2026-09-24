use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;

#[test]
fn draft_2020_12_validator_handles_unevaluated_properties_and_dynamic_refs() {
    let inspection = ModuleInspection {
        api_version: 1,
        id: "schema".to_string(),
        title: "Schema".to_string(),
        callable_name: "schema".to_string(),
        input_schema: json!({
            "$schema": DRAFT_2020_12,
            "$defs": {
                "base": {
                    "$dynamicAnchor": "node",
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" },
                        "next": { "$dynamicRef": "#node" }
                    },
                    "required": ["value"]
                }
            },
            "allOf": [{ "$ref": "#/$defs/base" }],
            "type": "object",
            "properties": { "workingDirectory": { "type": "string" } },
            "unevaluatedProperties": false
        }),
        output_schema: json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "additionalProperties": true
        }),
        has_complete: false,
    };
    let contract = contract_from_inspection(&inspection).expect("compile contract");
    assert!(
        contract
            .validate_input(&json!({
                "value": "ok",
                "next": { "value": "child" }
            }))
            .is_ok()
    );
    assert!(
        contract
            .validate_input(&json!({ "value": "ok", "extra": true }))
            .is_err()
    );
}

#[test]
fn contract_rejects_non_lower_camel_effective_input_properties() {
    for indirect_shape in [
        json!({
            "$defs": {
                "fields": {
                    "properties": { "snake_case": { "type": "string" } }
                }
            },
            "$ref": "#/$defs/fields"
        }),
        json!({
            "allOf": [{
                "properties": { "snake_case": { "type": "string" } }
            }]
        }),
        json!({
            "anyOf": [{
                "properties": { "snake_case": { "type": "string" } }
            }]
        }),
        json!({
            "oneOf": [{
                "properties": { "snake_case": { "type": "string" } }
            }]
        }),
    ] {
        let mut input_schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": { "workingDirectory": { "type": "string" } },
            "additionalProperties": true
        });
        input_schema
            .as_object_mut()
            .expect("input schema object")
            .extend(
                indirect_shape
                    .as_object()
                    .expect("indirect schema shape")
                    .clone(),
            );

        let error = WorkflowContract::from_schemas(
            input_schema,
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object"
            }),
        )
        .expect_err("effective properties must be lower camelCase");

        assert_eq!(
            error.to_string(),
            "workflow inputSchema property \"snake_case\" must be lower camelCase"
        );
    }
}

#[test]
fn contract_rejects_schemas_that_do_not_accept_injected_working_directory() {
    for working_directory_schema in [
        json!({ "type": "number" }),
        json!({ "anyOf": [{ "type": "number" }, { "const": false }] }),
    ] {
        let error = WorkflowContract::from_schemas(
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object",
                "properties": { "workingDirectory": working_directory_schema },
                "additionalProperties": false
            }),
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object"
            }),
        )
        .expect_err("injected workingDirectory must be accepted");
        assert_eq!(
            error.to_string(),
            "workflow inputSchema must accept workingDirectory as a string"
        );
    }
    let error = WorkflowContract::from_schemas(
        json!({
            "$schema": DRAFT_2020_12,
            "$defs": { "path": { "type": "number" } },
            "type": "object",
            "properties": { "workingDirectory": { "$ref": "#/$defs/path" } },
            "additionalProperties": false
        }),
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object"
        }),
    )
    .expect_err("referenced workingDirectory schema must accept strings");
    assert_eq!(
        error.to_string(),
        "workflow inputSchema must accept workingDirectory as a string"
    );

    for object_constraint in [
        json!({ "propertyNames": { "pattern": "^message$" } }),
        json!({
            "dependentRequired": {
                "workingDirectory": ["message"]
            }
        }),
        json!({
            "not": {
                "required": ["workingDirectory"]
            }
        }),
    ] {
        let mut schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "additionalProperties": true
        });
        schema
            .as_object_mut()
            .expect("object schema")
            .extend(object_constraint.as_object().expect("constraint").clone());
        let error = WorkflowContract::from_schemas(
            schema,
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object"
            }),
        )
        .expect_err("object constraint must permit injected workingDirectory");
        assert_eq!(
            error.to_string(),
            "workflow inputSchema must accept workingDirectory as a string"
        );
    }

    for object_constraint in [
        json!({ "maxProperties": 0 }),
        json!({
            "maxProperties": 1,
            "properties": {
                "message": { "type": "string" },
                "workingDirectory": { "type": "string" }
            }
        }),
        json!({
            "if": { "maxProperties": 1 },
            "then": true,
            "else": { "required": ["message"] }
        }),
    ] {
        let mut schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "additionalProperties": true
        });
        schema
            .as_object_mut()
            .expect("object schema")
            .extend(object_constraint.as_object().expect("constraint").clone());
        let error = WorkflowContract::from_schemas(
            schema,
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object"
            }),
        )
        .expect_err("top-level maxProperties must be rejected");
        assert_eq!(
            error.to_string(),
            "workflow inputSchema must not use top-level maxProperties because workingDirectory is injected"
        );
    }

    let error = WorkflowContract::from_schemas(
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "$ref": "#limit",
            "$defs": {
                "limited": {
                    "$anchor": "limit",
                    "type": "object",
                    "maxProperties": 1,
                    "properties": {
                        "message": { "type": "string" },
                        "workingDirectory": { "type": "string" }
                    }
                }
            }
        }),
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object"
        }),
    )
    .expect_err("anchored maxProperties must not bypass injection checks");
    assert_eq!(
        error.to_string(),
        "workflow inputSchema must not use top-level maxProperties because workingDirectory is injected"
    );

    let error = WorkflowContract::from_schemas(
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {
                "trigger": { "type": "boolean" },
                "workingDirectory": { "type": "string" }
            },
            "required": ["trigger"],
            "dependentSchemas": {
                "trigger": {
                    "not": { "required": ["workingDirectory"] }
                }
            },
            "additionalProperties": false
        }),
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object"
        }),
    )
    .expect_err("representative required input must remain valid after cwd injection");
    assert_eq!(
        error.to_string(),
        "workflow inputSchema must accept workingDirectory as a string"
    );

    let error = WorkflowContract::from_schemas(
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {
                "trigger": { "type": "boolean" },
                "workingDirectory": { "type": "string" }
            },
            "if": {
                "properties": { "trigger": { "const": true } },
                "required": ["trigger"]
            },
            "then": { "not": { "required": ["workingDirectory"] } },
            "additionalProperties": false
        }),
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object"
        }),
    )
    .expect_err("a value-triggered conditional must remain valid after cwd injection");
    assert_eq!(
        error.to_string(),
        "workflow inputSchema must accept workingDirectory as a string"
    );
}

#[test]
fn contract_allows_other_required_fields_alongside_working_directory() {
    for conditional in [
        json!({}),
        json!({
            "if": { "required": ["workingDirectory"] },
            "then": { "required": ["message"] },
            "else": { "required": ["message"] }
        }),
        json!({
            "not": { "maxProperties": 1 }
        }),
    ] {
        let mut schema = json!({
            "$schema": DRAFT_2020_12,
            "type": "object",
            "properties": {
                "workingDirectory": { "type": "string" },
                "message": { "type": "string" }
            },
            "required": ["message"],
            "additionalProperties": false
        });
        schema
            .as_object_mut()
            .expect("object schema")
            .extend(conditional.as_object().expect("conditional").clone());
        WorkflowContract::from_schemas(
            schema,
            json!({
                "$schema": DRAFT_2020_12,
                "type": "object"
            }),
        )
        .expect("unrelated required fields should not make injection incompatible");
    }
}

#[test]
fn contract_allows_working_directory_declared_through_composition() {
    WorkflowContract::from_schemas(
        json!({
            "$schema": DRAFT_2020_12,
            "$defs": {
                "injected": {
                    "properties": {
                        "workingDirectory": { "type": "string" }
                    }
                }
            },
            "type": "object",
            "allOf": [
                { "$ref": "#/$defs/injected" },
                {
                    "properties": { "message": { "type": "string" } },
                    "required": ["message"]
                }
            ],
            "unevaluatedProperties": false
        }),
        json!({
            "$schema": DRAFT_2020_12,
            "type": "object"
        }),
    )
    .expect("composed schemas may safely declare and evaluate workingDirectory");
}
