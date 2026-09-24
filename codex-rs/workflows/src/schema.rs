use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;
use jsonschema::Draft;
use jsonschema::ValidationError;
use jsonschema::Validator;
use jsonschema::error::ValidationErrorKind;
use serde_json::Value;

use crate::WorkflowManifest;
use crate::runner::ModuleInspection;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
const MAX_TOP_LEVEL_INPUT_PROPERTIES: usize = 128;

#[derive(Clone, Debug)]
pub struct WorkflowContract {
    input_schema: Value,
    output_schema: Value,
    input_validator: Validator,
    output_validator: Validator,
}

impl WorkflowContract {
    pub fn from_schemas(input_schema: Value, output_schema: Value) -> anyhow::Result<Self> {
        require_draft(&input_schema, "inputSchema")?;
        require_draft(&output_schema, "outputSchema")?;
        let input_types = input_schema
            .get("type")
            .map(|value| match value {
                Value::String(value) => vec![value.as_str()],
                Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        if !input_types.contains(&"object") {
            bail!("workflow inputSchema must describe a JSON object");
        }
        if root_schema_uses_max_properties(&input_schema, &input_schema, &mut BTreeSet::new()) {
            bail!(
                "workflow inputSchema must not use top-level maxProperties because workingDirectory is injected"
            );
        }
        let input_validator = compile_schema(&input_schema, "inputSchema")?;
        validate_working_directory_injection(&input_schema, &input_validator)?;
        let output_validator = compile_schema(&output_schema, "outputSchema")?;
        Ok(Self {
            input_schema,
            output_schema,
            input_validator,
            output_validator,
        })
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn output_schema(&self) -> &Value {
        &self.output_schema
    }

    pub fn validate_input(&self, input: &Value) -> Result<(), String> {
        validate_instance(&self.input_validator, input, "Workflow input")
    }

    pub fn validate_output(&self, output: &Value) -> Result<(), String> {
        validate_instance(&self.output_validator, output, "Workflow output")
    }
}

fn root_schema_uses_max_properties(
    root: &Value,
    schema: &Value,
    visited_refs: &mut BTreeSet<String>,
) -> bool {
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema.contains_key("maxProperties") {
        return true;
    }
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str)
            && visited_refs.insert(format!("{keyword}:{reference}"))
            && let Some(referenced) = resolve_local_schema_ref(root, reference)
            && root_schema_uses_max_properties(root, referenced, visited_refs)
        {
            return true;
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if schema
            .get(keyword)
            .and_then(Value::as_array)
            .is_some_and(|schemas| {
                schemas
                    .iter()
                    .any(|schema| root_schema_uses_max_properties(root, schema, visited_refs))
            })
        {
            return true;
        }
    }
    for keyword in ["if", "then", "else"] {
        if schema
            .get(keyword)
            .is_some_and(|schema| root_schema_uses_max_properties(root, schema, visited_refs))
        {
            return true;
        }
    }
    schema
        .get("dependentSchemas")
        .and_then(Value::as_object)
        .is_some_and(|schemas| {
            schemas
                .values()
                .any(|schema| root_schema_uses_max_properties(root, schema, visited_refs))
        })
}

fn resolve_local_schema_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    if reference == "#" {
        return Some(root);
    }
    let fragment = reference.strip_prefix('#')?;
    if fragment.starts_with('/') {
        root.pointer(fragment)
    } else {
        find_schema_anchor(root, fragment)
    }
}

fn find_schema_anchor<'a>(schema: &'a Value, anchor: &str) -> Option<&'a Value> {
    match schema {
        Value::Object(object) => {
            if ["$anchor", "$dynamicAnchor"]
                .into_iter()
                .any(|keyword| object.get(keyword).and_then(Value::as_str) == Some(anchor))
            {
                return Some(schema);
            }
            object
                .values()
                .find_map(|value| find_schema_anchor(value, anchor))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_schema_anchor(value, anchor)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn validate_working_directory_injection(
    schema: &Value,
    validator: &Validator,
) -> anyhow::Result<()> {
    let baseline = validation_signatures(validator, &serde_json::json!({}));
    let injected = validation_signatures(
        validator,
        &serde_json::json!({ "workingDirectory": "/codex/workflow" }),
    );
    if !injected.is_subset(&baseline) {
        bail!("workflow inputSchema must accept workingDirectory as a string");
    }
    let mut properties = BTreeMap::new();
    let mut required = BTreeSet::new();
    collect_top_level_object_shape(
        schema,
        schema,
        &mut BTreeSet::new(),
        &mut properties,
        &mut required,
    );
    if properties.len() > MAX_TOP_LEVEL_INPUT_PROPERTIES {
        bail!(
            "workflow inputSchema exposes more than {MAX_TOP_LEVEL_INPUT_PROPERTIES} top-level properties"
        );
    }
    let mut required_input = serde_json::Map::new();
    for name in &required {
        if name != "workingDirectory" {
            let value = properties
                .get(name)
                .map(|property| schema_candidate(schema, property))
                .unwrap_or(Value::Null);
            required_input.insert(name.clone(), value);
        }
    }
    let mut probes = vec![Value::Object(required_input.clone())];
    for (name, property) in &properties {
        if name == "workingDirectory" {
            continue;
        }
        let mut probe = required_input.clone();
        probe.insert(name.clone(), schema_candidate(schema, property));
        probes.push(Value::Object(probe));
    }
    let mut all_properties = required_input;
    for (name, property) in &properties {
        if name != "workingDirectory" {
            all_properties
                .entry(name.clone())
                .or_insert_with(|| schema_candidate(schema, property));
        }
    }
    probes.push(Value::Object(all_properties));
    for baseline in probes {
        if !validator.is_valid(&baseline) {
            continue;
        }
        let mut injected = baseline;
        let Value::Object(injected_object) = &mut injected else {
            bail!("internal workflow injection probe was not an object");
        };
        injected_object.insert(
            "workingDirectory".to_string(),
            Value::String("/codex/workflow".to_string()),
        );
        if !validator.is_valid(&injected) {
            bail!("workflow inputSchema must accept workingDirectory as a string");
        }
    }
    Ok(())
}

fn collect_top_level_object_shape<'a>(
    root: &'a Value,
    schema: &'a Value,
    visited_refs: &mut BTreeSet<String>,
    properties: &mut BTreeMap<String, &'a Value>,
    required: &mut BTreeSet<String>,
) {
    let Some(schema) = schema.as_object() else {
        return;
    };
    if let Some(schema_properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, property) in schema_properties {
            properties.entry(name.clone()).or_insert(property);
        }
    }
    if let Some(schema_required) = schema.get("required").and_then(Value::as_array) {
        required.extend(
            schema_required
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str)
            && visited_refs.insert(format!("{keyword}:{reference}"))
            && let Some(referenced) = resolve_local_schema_ref(root, reference)
        {
            collect_top_level_object_shape(root, referenced, visited_refs, properties, required);
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                collect_top_level_object_shape(root, branch, visited_refs, properties, required);
            }
        }
    }
}

fn schema_candidate(root: &Value, schema: &Value) -> Value {
    schema_candidate_inner(root, schema, &mut BTreeSet::new(), /*depth*/ 0)
}

fn schema_candidate_inner(
    root: &Value,
    schema: &Value,
    visited_refs: &mut BTreeSet<String>,
    depth: usize,
) -> Value {
    if depth >= 32 {
        return Value::Null;
    }
    let Some(schema) = schema.as_object() else {
        return Value::Null;
    };
    for keyword in ["const", "default"] {
        if let Some(value) = schema.get(keyword) {
            return value.clone();
        }
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str)
            && visited_refs.insert(format!("{keyword}:{reference}"))
            && let Some(referenced) = resolve_local_schema_ref(root, reference)
        {
            return schema_candidate_inner(root, referenced, visited_refs, depth + 1);
        }
    }
    if let Some(candidate) = ["allOf", "anyOf", "oneOf"].into_iter().find_map(|keyword| {
        schema
            .get(keyword)
            .and_then(Value::as_array)
            .and_then(|branches| branches.first())
    }) {
        return schema_candidate_inner(root, candidate, visited_refs, depth + 1);
    }
    let schema_type = schema
        .get("type")
        .and_then(|schema_type| match schema_type {
            Value::String(value) => Some(value.as_str()),
            Value::Array(values) => values.iter().find_map(Value::as_str),
            _ => None,
        });
    match schema_type {
        Some("string") => Value::String(String::new()),
        Some("number" | "integer") => serde_json::json!(0),
        Some("boolean") => Value::Bool(false),
        Some("object") => Value::Object(serde_json::Map::new()),
        Some("array") => Value::Array(Vec::new()),
        Some("null") | None | Some(_) => Value::Null,
    }
}

fn validation_signatures(validator: &Validator, instance: &Value) -> BTreeSet<String> {
    let mut signatures = BTreeSet::new();
    for error in validator.iter_errors(instance) {
        collect_validation_signatures(&error, &mut signatures);
    }
    signatures
}

fn collect_validation_signatures(error: &ValidationError<'_>, signatures: &mut BTreeSet<String>) {
    let detail = match error.kind() {
        ValidationErrorKind::AnyOf { .. }
        | ValidationErrorKind::OneOfMultipleValid { .. }
        | ValidationErrorKind::OneOfNotValid { .. }
        | ValidationErrorKind::PropertyNames { .. } => error.kind().keyword().to_string(),
        kind => format!("{kind:?}"),
    };
    signatures.insert(format!("{}|{detail}", error.instance_path(),));
    match error.kind() {
        ValidationErrorKind::AnyOf { context }
        | ValidationErrorKind::OneOfMultipleValid { context }
        | ValidationErrorKind::OneOfNotValid { context } => {
            for branch in context {
                for nested in branch {
                    collect_validation_signatures(nested, signatures);
                }
            }
        }
        ValidationErrorKind::PropertyNames { error } => {
            collect_validation_signatures(error, signatures);
        }
        _ => {}
    }
}

pub fn load_workflow_contract(
    workflow_dir: &Path,
    expected: &WorkflowManifest,
) -> anyhow::Result<WorkflowContract> {
    let inspection = crate::runner::inspect_workflow(workflow_dir, expected)?;
    contract_from_inspection(&inspection)
}

pub(crate) fn contract_from_inspection(
    inspection: &ModuleInspection,
) -> anyhow::Result<WorkflowContract> {
    WorkflowContract::from_schemas(
        inspection.input_schema.clone(),
        inspection.output_schema.clone(),
    )
}

fn require_draft(schema: &Value, label: &str) -> anyhow::Result<()> {
    if schema.get("$schema").and_then(Value::as_str) != Some(DRAFT_2020_12) {
        bail!("workflow {label} must explicitly declare Draft 2020-12");
    }
    Ok(())
}

fn compile_schema(schema: &Value, label: &str) -> anyhow::Result<Validator> {
    jsonschema::meta::validate(schema).map_err(|err| {
        anyhow::anyhow!("workflow {label} is not a valid Draft 2020-12 schema: {err}")
    })?;
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .with_context(|| format!("failed to compile workflow {label}"))
}

fn validate_instance(validator: &Validator, instance: &Value, label: &str) -> Result<(), String> {
    let errors = validator
        .iter_errors(instance)
        .take(/*n*/ 20)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{label} failed schema validation: {}",
            errors.join("; ")
        ))
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
