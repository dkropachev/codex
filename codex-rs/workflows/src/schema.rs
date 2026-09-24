use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use anyhow::bail;
use jsonschema::Draft;
use jsonschema::Validator;
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
        validate_working_directory_injection(&input_schema)?;
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

fn validate_working_directory_injection(schema: &Value) -> anyhow::Result<()> {
    if !injection_effect(schema, schema, &mut BTreeSet::new()).preserves_validity {
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
    if let Some(name) = properties.keys().find(|name| !is_lower_camel_case(name)) {
        bail!("workflow inputSchema property {name:?} must be lower camelCase");
    }
    if properties.len() > MAX_TOP_LEVEL_INPUT_PROPERTIES {
        bail!(
            "workflow inputSchema exposes more than {MAX_TOP_LEVEL_INPUT_PROPERTIES} top-level properties"
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct InjectionEffect {
    preserves_validity: bool,
    preserves_invalidity: bool,
}

impl InjectionEffect {
    const INVARIANT: Self = Self {
        preserves_validity: true,
        preserves_invalidity: true,
    };
    const UNKNOWN: Self = Self {
        preserves_validity: false,
        preserves_invalidity: false,
    };

    fn and(self, other: Self) -> Self {
        Self {
            preserves_validity: self.preserves_validity && other.preserves_validity,
            preserves_invalidity: self.preserves_invalidity && other.preserves_invalidity,
        }
    }

    fn is_invariant(self) -> bool {
        self.preserves_validity && self.preserves_invalidity
    }
}

/// Conservatively proves how a schema's truth value changes when a string-valued
/// `workingDirectory` property is added to an object that did not have one.
/// Both directions are needed for `not`, `oneOf`, and conditional predicates.
fn injection_effect(
    root: &Value,
    schema: &Value,
    active_refs: &mut BTreeSet<String>,
) -> InjectionEffect {
    let schema_value = schema;
    let Some(schema) = schema.as_object() else {
        return InjectionEffect::INVARIANT;
    };
    let mut effect = InjectionEffect::INVARIANT;

    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str) {
            let key = format!("{keyword}:{reference}");
            let reference_effect = if active_refs.insert(key.clone()) {
                let value = resolve_local_schema_ref(root, reference)
                    .map(|referenced| injection_effect(root, referenced, active_refs))
                    .unwrap_or(InjectionEffect::UNKNOWN);
                active_refs.remove(&key);
                value
            } else {
                InjectionEffect::UNKNOWN
            };
            effect = effect.and(reference_effect);
        }
    }

    if let Some(value) = schema.get("const") {
        effect = effect.and(exact_value_effect(std::slice::from_ref(value)));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        effect = effect.and(exact_value_effect(values));
    }
    if schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| required.iter().any(|name| name == "workingDirectory"))
    {
        effect.preserves_invalidity = false;
    }
    if schema
        .get("minProperties")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| minimum > 0)
    {
        effect.preserves_invalidity = false;
    }
    if schema.get("maxProperties").is_some() {
        effect.preserves_validity = false;
    }

    let declares_working_directory = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get("workingDirectory"));
    if declares_working_directory
        .is_some_and(|property| !schema_accepts_every_string(root, property, active_refs))
    {
        effect.preserves_validity = false;
    }
    let matching_patterns = schema
        .get("patternProperties")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|patterns| patterns.iter())
        .filter(|(pattern, _)| pattern_matches_working_directory(pattern))
        .collect::<Vec<_>>();
    if matching_patterns
        .iter()
        .any(|(_, property)| !schema_accepts_every_string(root, property, active_refs))
    {
        effect.preserves_validity = false;
    }
    if declares_working_directory.is_none()
        && matching_patterns.is_empty()
        && schema
            .get("additionalProperties")
            .is_some_and(|additional| !schema_accepts_every_string(root, additional, active_refs))
    {
        effect.preserves_validity = false;
    }
    if let Some(unevaluated) = schema.get("unevaluatedProperties")
        && !schema_guarantees_working_directory_evaluated(root, schema_value, active_refs)
        && !schema_accepts_every_string(root, unevaluated, active_refs)
    {
        effect.preserves_validity = false;
    }
    if schema.get("propertyNames").is_some_and(|names| {
        !schema_accepts_known_string(root, names, "workingDirectory", active_refs)
    }) {
        effect.preserves_validity = false;
    }

    if let Some(dependencies) = schema.get("dependentRequired").and_then(Value::as_object) {
        for (trigger, required) in dependencies {
            let requires_working_directory = required
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "workingDirectory"));
            if trigger == "workingDirectory" {
                if required
                    .as_array()
                    .is_some_and(|names| names.iter().any(|name| name != "workingDirectory"))
                {
                    effect.preserves_validity = false;
                }
            } else if requires_working_directory {
                effect.preserves_invalidity = false;
            }
        }
    }
    if let Some(dependencies) = schema.get("dependentSchemas").and_then(Value::as_object) {
        for (trigger, dependency) in dependencies {
            if trigger == "workingDirectory" {
                if !schema_is_obviously_true(dependency) {
                    effect.preserves_validity = false;
                }
            } else {
                effect = effect.and(injection_effect(root, dependency, active_refs));
            }
        }
    }

    for keyword in ["allOf", "anyOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                effect = effect.and(injection_effect(root, branch, active_refs));
            }
        }
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array)
        && !branches
            .iter()
            .all(|branch| injection_effect(root, branch, active_refs).is_invariant())
    {
        effect = effect.and(InjectionEffect::UNKNOWN);
    }
    if let Some(negated) = schema.get("not") {
        let negated = injection_effect(root, negated, active_refs);
        effect = effect.and(InjectionEffect {
            preserves_validity: negated.preserves_invalidity,
            preserves_invalidity: negated.preserves_validity,
        });
    }
    if let Some(condition) = schema.get("if") {
        let then_schema = schema.get("then");
        let else_schema = schema.get("else");
        let then_effect = then_schema
            .map(|branch| injection_effect(root, branch, active_refs))
            .unwrap_or(InjectionEffect::INVARIANT);
        let else_effect = else_schema
            .map(|branch| injection_effect(root, branch, active_refs))
            .unwrap_or(InjectionEffect::INVARIANT);
        let branch_effect = if then_schema == else_schema {
            then_effect
        } else if injection_effect(root, condition, active_refs).is_invariant() {
            then_effect.and(else_effect)
        } else {
            InjectionEffect::UNKNOWN
        };
        effect = effect.and(branch_effect);
    }
    effect
}

fn exact_value_effect(values: &[Value]) -> InjectionEffect {
    let mut effect = InjectionEffect::INVARIANT;
    for value in values {
        let Some(object) = value.as_object() else {
            continue;
        };
        match object.get("workingDirectory") {
            None => effect.preserves_validity = false,
            Some(Value::String(_)) => effect.preserves_invalidity = false,
            Some(_) => {}
        }
    }
    effect
}

fn schema_is_obviously_true(schema: &Value) -> bool {
    schema == &Value::Bool(true) || schema.as_object().is_some_and(serde_json::Map::is_empty)
}

fn is_lower_camel_case(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
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

fn schema_accepts_every_string(
    root: &Value,
    schema: &Value,
    active_refs: &mut BTreeSet<String>,
) -> bool {
    if let Some(value) = schema.as_bool() {
        return value;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema.contains_key("const") || schema.contains_key("enum") {
        return false;
    }
    if let Some(schema_type) = schema.get("type")
        && !schema_type_allows(schema_type, "string")
    {
        return false;
    }
    if schema
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|length| length > 0)
        || schema.get("maxLength").is_some()
        || schema
            .get("pattern")
            .and_then(Value::as_str)
            .is_some_and(|pattern| !pattern.is_empty())
        || schema.contains_key("format")
    {
        return false;
    }
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str) {
            let key = format!("string:{keyword}:{reference}");
            if !active_refs.insert(key.clone()) {
                return false;
            }
            let accepts = resolve_local_schema_ref(root, reference).is_some_and(|referenced| {
                schema_accepts_every_string(root, referenced, active_refs)
            });
            active_refs.remove(&key);
            if !accepts {
                return false;
            }
        }
    }
    if schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches
                .iter()
                .any(|branch| !schema_accepts_every_string(root, branch, active_refs))
        })
    {
        return false;
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array)
        && !branches
            .iter()
            .any(|branch| schema_accepts_every_string(root, branch, active_refs))
    {
        return false;
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        let universal = branches
            .iter()
            .filter(|branch| schema_accepts_every_string(root, branch, active_refs))
            .count();
        if universal != 1
            || !branches.iter().all(|branch| {
                schema_accepts_every_string(root, branch, active_refs)
                    || schema_rejects_every_string(root, branch, active_refs)
            })
        {
            return false;
        }
    }
    if schema
        .get("not")
        .is_some_and(|negated| !schema_rejects_every_string(root, negated, active_refs))
    {
        return false;
    }
    if let Some(condition) = schema.get("if") {
        let condition_accepts = schema_accepts_every_string(root, condition, active_refs);
        let condition_rejects = schema_rejects_every_string(root, condition, active_refs);
        let then_accepts = schema
            .get("then")
            .is_none_or(|branch| schema_accepts_every_string(root, branch, active_refs));
        let else_accepts = schema
            .get("else")
            .is_none_or(|branch| schema_accepts_every_string(root, branch, active_refs));
        if !((condition_accepts && then_accepts)
            || (condition_rejects && else_accepts)
            || (then_accepts && else_accepts))
        {
            return false;
        }
    }
    true
}

fn schema_rejects_every_string(
    root: &Value,
    schema: &Value,
    active_refs: &mut BTreeSet<String>,
) -> bool {
    if schema == &Value::Bool(false) {
        return true;
    }
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema
        .get("type")
        .is_some_and(|schema_type| !schema_type_allows(schema_type, "string"))
        || schema.get("const").is_some_and(|value| !value.is_string())
        || schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().all(|value| !value.is_string()))
    {
        return true;
    }
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches
                .iter()
                .any(|branch| schema_rejects_every_string(root, branch, active_refs))
        })
        || schema
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                branches
                    .iter()
                    .all(|branch| schema_rejects_every_string(root, branch, active_refs))
            })
        || schema
            .get("not")
            .is_some_and(|negated| schema_accepts_every_string(root, negated, active_refs))
}

fn schema_accepts_known_string(
    root: &Value,
    schema: &Value,
    value: &str,
    active_refs: &mut BTreeSet<String>,
) -> bool {
    let probe = serde_json::json!({
        "$schema": DRAFT_2020_12,
        "type": "string",
        "const": value,
        "allOf": [schema]
    });
    if !contains_local_ref(schema) {
        return jsonschema::options()
            .with_draft(Draft::Draft202012)
            .build(&probe)
            .is_ok_and(|validator| validator.is_valid(&Value::String(value.to_string())));
    }
    schema_accepts_every_string(root, schema, active_refs)
}

fn contains_local_ref(schema: &Value) -> bool {
    match schema {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(key.as_str(), "$ref" | "$dynamicRef") || contains_local_ref(value)
        }),
        Value::Array(values) => values.iter().any(contains_local_ref),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn schema_type_allows(schema_type: &Value, expected: &str) -> bool {
    schema_type == expected
        || schema_type
            .as_array()
            .is_some_and(|types| types.iter().any(|schema_type| schema_type == expected))
}

fn pattern_matches_working_directory(pattern: &str) -> bool {
    let schema = serde_json::json!({
        "$schema": DRAFT_2020_12,
        "type": "string",
        "pattern": pattern
    });
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .map_or(true, |validator| {
            validator.is_valid(&Value::String("workingDirectory".to_string()))
        })
}

fn schema_guarantees_working_directory_evaluated(
    root: &Value,
    schema: &Value,
    active_refs: &mut BTreeSet<String>,
) -> bool {
    let Some(schema) = schema.as_object() else {
        return false;
    };
    if schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| properties.contains_key("workingDirectory"))
        || schema
            .get("patternProperties")
            .and_then(Value::as_object)
            .is_some_and(|patterns| {
                patterns
                    .keys()
                    .any(|pattern| pattern_matches_working_directory(pattern))
            })
        || schema.contains_key("additionalProperties")
    {
        return true;
    }
    for keyword in ["$ref", "$dynamicRef"] {
        if let Some(reference) = schema.get(keyword).and_then(Value::as_str) {
            let key = format!("evaluated:{keyword}:{reference}");
            if active_refs.insert(key.clone()) {
                let evaluated =
                    resolve_local_schema_ref(root, reference).is_some_and(|referenced| {
                        schema_guarantees_working_directory_evaluated(root, referenced, active_refs)
                    });
                active_refs.remove(&key);
                if evaluated {
                    return true;
                }
            }
        }
    }
    schema
        .get("allOf")
        .and_then(Value::as_array)
        .is_some_and(|branches| {
            branches.iter().any(|branch| {
                schema_guarantees_working_directory_evaluated(root, branch, active_refs)
            })
        })
        || ["anyOf", "oneOf"].into_iter().any(|keyword| {
            schema
                .get(keyword)
                .and_then(Value::as_array)
                .is_some_and(|branches| {
                    !branches.is_empty()
                        && branches.iter().all(|branch| {
                            schema_guarantees_working_directory_evaluated(root, branch, active_refs)
                        })
                })
        })
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
