use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde_json::Value as JsonValue;

use crate::api_contract::WorkflowSourceContract;
use crate::registry::WorkflowSummary;

const GENERATED_WORKFLOW_MODULE_PATH: &str = "src/generated/workflows.ts";

pub(crate) fn generate_workflow_client_modules(
    workflows: &[WorkflowSummary],
    source_contracts: &BTreeMap<PathBuf, WorkflowSourceContract>,
) -> Result<()> {
    let callable_workflows = collect_callable_workflows(workflows, source_contracts)?;

    for consumer in workflows {
        let module_source = render_generated_workflow_module(consumer, &callable_workflows)?;
        let module_path = consumer.path.join(GENERATED_WORKFLOW_MODULE_PATH);
        if let Some(parent) = module_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create generated workflow directory {}",
                    parent.display()
                )
            })?;
        }
        fs::write(&module_path, module_source).with_context(|| {
            format!(
                "failed to write generated workflow module {}",
                module_path.display()
            )
        })?;
    }

    Ok(())
}

struct CallableWorkflow<'a> {
    workflow: &'a WorkflowSummary,
    contract: &'a WorkflowSourceContract,
}

fn collect_callable_workflows<'a>(
    workflows: &'a [WorkflowSummary],
    source_contracts: &'a BTreeMap<PathBuf, WorkflowSourceContract>,
) -> Result<Vec<CallableWorkflow<'a>>> {
    let mut callables = Vec::new();
    let mut callable_names = BTreeMap::<&str, &WorkflowSummary>::new();

    for workflow in workflows {
        let Some(contract) = source_contracts.get(&workflow.path) else {
            continue;
        };
        let Some(callable_name) = contract.callable_name.as_deref() else {
            continue;
        };
        if let Some(previous) = callable_names.insert(callable_name, workflow)
            && previous.path != workflow.path
        {
            return Err(anyhow!(
                "duplicate workflow callable name `{callable_name}` in {} and {}",
                previous.path.display(),
                workflow.path.display()
            ));
        }
        callables.push(CallableWorkflow { workflow, contract });
    }

    callables.sort_by(|left, right| {
        left.contract
            .callable_name
            .cmp(&right.contract.callable_name)
            .then_with(|| left.workflow.id.cmp(&right.workflow.id))
    });
    Ok(callables)
}

fn render_generated_workflow_module(
    consumer: &WorkflowSummary,
    callables: &[CallableWorkflow<'_>],
) -> Result<String> {
    if callables.is_empty() {
        return Ok("export {};\n".to_string());
    }

    let module_path = consumer.path.join(GENERATED_WORKFLOW_MODULE_PATH);
    let mut output = String::new();
    output.push_str("import { CodexWorkflow } from \"@openai/codex-sdk/workflow\";\n");

    let mut formatter_imports = BTreeMap::<String, FormatterImport>::new();
    for callable in callables {
        if callable
            .contract
            .format_schemas
            .contains_key("tui.markdown.v1")
        {
            let callable_name = callable
                .contract
                .callable_name
                .as_deref()
                .ok_or_else(|| anyhow!("callable workflows always have callable names"))?;
            let companion_name = format!("{}WorkflowOutput", pascal_case(callable_name));
            let import_path = relative_import_path(
                &module_path,
                &callable.workflow.path.join("src/workflow.js"),
            )?;
            formatter_imports.insert(
                companion_name,
                FormatterImport {
                    import_path,
                    workflow_alias: format!("{}Workflow", pascal_case(callable_name)),
                    format_hook: callable.contract.format_hook,
                },
            );
        }
    }

    for (alias, formatter_import) in &formatter_imports {
        if formatter_import.format_hook {
            writeln!(
                output,
                "import {} from \"{}\";",
                formatter_import.workflow_alias, formatter_import.import_path
            )?;
        } else {
            writeln!(
                output,
                "import {{ WorkflowOutput as {alias} }} from \"{}\";",
                formatter_import.import_path
            )?;
        }
    }
    if !formatter_imports.is_empty() {
        output.push('\n');
    }

    output.push_str(SHARED_RUNTIME_HELPERS);
    output.push('\n');

    for callable in callables {
        let callable_name = callable
            .contract
            .callable_name
            .as_deref()
            .ok_or_else(|| anyhow!("callable workflows always have callable names"))?;
        let pascal_name = pascal_case(callable_name);
        let input_type_name = format!("{pascal_name}Input");
        let output_type_name = format!("{pascal_name}Output");
        let input_schema_name = format!("{callable_name}InputSchema");
        let output_schema_name = format!("{callable_name}OutputSchema");
        let input_ts_type = render_ts_type(&callable.contract.input_schema)?;
        let output_ts_type = render_ts_type(&callable.contract.output_schema)?;
        let formatter_alias = format!("{pascal_name}WorkflowOutput");
        let workflow_id = &callable.workflow.id;

        writeln!(output, "export type {input_type_name} = {input_ts_type};")?;
        writeln!(output, "export type {output_type_name} = {output_ts_type};")?;
        writeln!(
            output,
            "export const {input_schema_name} = {} as const;",
            json_literal(&callable.contract.input_schema)?,
        )?;
        writeln!(
            output,
            "export const {output_schema_name} = {} as const;",
            json_literal(&callable.contract.output_schema)?,
        )?;
        writeln!(
            output,
            "export async function {callable_name}(input: {input_type_name}): Promise<{output_type_name}> {{"
        )?;
        writeln!(
            output,
            "  return runValidatedWorkflow<{output_type_name}>(\"{workflow_id}\", {input_schema_name}, {output_schema_name}, input);"
        )?;
        output.push_str("}\n\n");

        if let Some(formatter_import) = formatter_imports.get(&formatter_alias) {
            if formatter_import.format_hook {
                let workflow_alias = &formatter_import.workflow_alias;
                write!(
                    output,
                    "export const {pascal_name}Output = {{\n  toTuiMarkdown(result: {output_type_name}): {{ markdown: string }} | Promise<{{ markdown: string }}> {{\n    const formatter = ({workflow_alias} as {{ format?: (result: never, request: {{ format: \"tui.markdown.v1\" }}) => {{ markdown: string }} | Promise<{{ markdown: string }}> }}).format;\n    if (typeof formatter !== \"function\") {{\n      throw new WorkflowContractError(\"{workflow_id}\", \"output\", \"$\", \"workflow format hook is missing\");\n    }}\n    return formatter(result as never, {{ format: \"tui.markdown.v1\" }});\n  }},\n}};\n"
                )?;
            } else {
                write!(
                    output,
                    "export const {pascal_name}Output = {{\n  toTuiMarkdown(result: {output_type_name}): {{ markdown: string }} {{\n    return {formatter_alias}.toTuiMarkdown(result as never);\n  }},\n}};\n"
                )?;
            }
        }
    }

    Ok(output)
}

struct FormatterImport {
    import_path: String,
    workflow_alias: String,
    format_hook: bool,
}

const SHARED_RUNTIME_HELPERS: &str = r##"
export type JsonSchema = {
  type?: string | readonly string[];
  enum?: readonly unknown[];
  anyOf?: readonly JsonSchema[];
  properties?: Record<string, JsonSchema>;
  required?: readonly string[];
  additionalProperties?: false | true | JsonSchema;
  items?: JsonSchema;
  prefixItems?: readonly JsonSchema[];
  minItems?: number;
  maxItems?: number;
  minimum?: number;
  maximum?: number;
};

export class WorkflowContractError extends Error {
  readonly workflowId: string;
  readonly direction: "input" | "output";
  readonly schemaPath: string;

  constructor(workflowId: string, direction: "input" | "output", schemaPath: string, message: string) {
    super(`workflow ${workflowId} ${direction} contract violation at ${schemaPath}: ${message}`);
    this.name = "WorkflowContractError";
    this.workflowId = workflowId;
    this.direction = direction;
    this.schemaPath = schemaPath;
  }
}

function failContract(workflowId: string, direction: "input" | "output", schemaPath: string, message: string): never {
  throw new WorkflowContractError(workflowId, direction, schemaPath, message);
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function validateContractValue(
  workflowId: string,
  direction: "input" | "output",
  schema: JsonSchema,
  value: unknown,
  schemaPath = "$",
): void {
  if (schema.anyOf?.length) {
    for (const candidate of schema.anyOf) {
      try {
        validateContractValue(workflowId, direction, candidate, value, schemaPath);
        return;
      } catch {
        // Try the next branch.
      }
    }
    failContract(workflowId, direction, schemaPath, "value does not match any allowed schema");
  }

  if (schema.enum?.length && value !== null) {
    if (!schema.enum.some((candidate) => Object.is(candidate, value))) {
      failContract(workflowId, direction, schemaPath, `value must be one of ${JSON.stringify(schema.enum)}`);
    }
  }

  const typeSpec = schema.type;
  if (typeof typeSpec === "string") {
    validateTypedValue(workflowId, direction, typeSpec, schema, value, schemaPath);
    return;
  }
  if (Array.isArray(typeSpec)) {
    if (typeSpec.includes("null") && value === null) {
      return;
    }
    for (const typeName of typeSpec) {
      if (matchesJsonType(typeName, value)) {
        validateTypedValue(workflowId, direction, typeName, schema, value, schemaPath);
        return;
      }
    }
    failContract(workflowId, direction, schemaPath, "value does not match any allowed type");
  }

  if (schema.properties || schema.additionalProperties !== undefined) {
    validateObject(workflowId, direction, schema, value, schemaPath);
    return;
  }
  if (schema.prefixItems || schema.items) {
    validateArray(workflowId, direction, schema, value, schemaPath);
  }
}

function validateTypedValue(
  workflowId: string,
  direction: "input" | "output",
  typeName: string,
  schema: JsonSchema,
  value: unknown,
  schemaPath: string,
): void {
  switch (typeName) {
    case "string":
      if (typeof value !== "string") {
        failContract(workflowId, direction, schemaPath, "expected string");
      }
      return;
    case "number":
      if (typeof value !== "number") {
        failContract(workflowId, direction, schemaPath, "expected number");
      }
      validateNumberBounds(workflowId, direction, schema, value, schemaPath);
      return;
    case "integer":
      if (typeof value !== "number" || !Number.isInteger(value)) {
        failContract(workflowId, direction, schemaPath, "expected integer");
      }
      validateNumberBounds(workflowId, direction, schema, value, schemaPath);
      return;
    case "boolean":
      if (typeof value !== "boolean") {
        failContract(workflowId, direction, schemaPath, "expected boolean");
      }
      return;
    case "null":
      if (value !== null) {
        failContract(workflowId, direction, schemaPath, "expected null");
      }
      return;
    case "object":
      validateObject(workflowId, direction, schema, value, schemaPath);
      return;
    case "array":
      validateArray(workflowId, direction, schema, value, schemaPath);
      return;
    default:
      failContract(workflowId, direction, schemaPath, `unsupported schema type ${typeName}`);
  }
}

function validateNumberBounds(
  workflowId: string,
  direction: "input" | "output",
  schema: JsonSchema,
  value: number,
  schemaPath: string,
): void {
  if (!Number.isFinite(value)) {
    failContract(workflowId, direction, schemaPath, "expected finite number");
  }
  if (schema.minimum !== undefined && value < schema.minimum) {
    failContract(workflowId, direction, schemaPath, `expected number >= ${schema.minimum}`);
  }
  if (schema.maximum !== undefined && value > schema.maximum) {
    failContract(workflowId, direction, schemaPath, `expected number <= ${schema.maximum}`);
  }
}

function validateObject(
  workflowId: string,
  direction: "input" | "output",
  schema: JsonSchema,
  value: unknown,
  schemaPath: string,
): void {
  if (!isPlainObject(value)) {
    failContract(workflowId, direction, schemaPath, "expected object");
  }

  const properties = schema.properties ?? {};
  const required = new Set(schema.required ?? []);
  for (const name of required) {
    if (!(name in value)) {
      failContract(workflowId, direction, `${schemaPath}.${name}`, "missing required property");
    }
  }

  for (const [name, itemValue] of Object.entries(value)) {
    const propertySchema = properties[name];
    if (propertySchema) {
      validateContractValue(workflowId, direction, propertySchema, itemValue, `${schemaPath}.${name}`);
      continue;
    }

    const additionalProperties = schema.additionalProperties;
    if (additionalProperties === false || additionalProperties === undefined) {
      failContract(workflowId, direction, `${schemaPath}.${name}`, "unexpected property");
    }
    if (additionalProperties === true) {
      continue;
    }
    validateContractValue(workflowId, direction, additionalProperties, itemValue, `${schemaPath}.${name}`);
  }
}

function validateArray(
  workflowId: string,
  direction: "input" | "output",
  schema: JsonSchema,
  value: unknown,
  schemaPath: string,
): void {
  if (!Array.isArray(value)) {
    failContract(workflowId, direction, schemaPath, "expected array");
  }

  const prefixItems = schema.prefixItems ?? [];
  const minItems = schema.minItems ?? prefixItems.length;
  const maxItems = schema.maxItems ?? (schema.items ? Number.POSITIVE_INFINITY : prefixItems.length);
  if (value.length < minItems || value.length > maxItems) {
    failContract(workflowId, direction, schemaPath, `expected between ${minItems} and ${maxItems} items`);
  }

  prefixItems.forEach((itemSchema, index) => {
    validateContractValue(workflowId, direction, itemSchema, value[index], `${schemaPath}[${index}]`);
  });

  if (schema.items) {
    for (let index = prefixItems.length; index < value.length; index += 1) {
      validateContractValue(workflowId, direction, schema.items, value[index], `${schemaPath}[${index}]`);
    }
  }
}

function matchesJsonType(typeName: string, value: unknown): boolean {
  switch (typeName) {
    case "string":
      return typeof value === "string";
    case "number":
      return typeof value === "number";
    case "integer":
      return typeof value === "number" && Number.isInteger(value);
    case "boolean":
      return typeof value === "boolean";
    case "null":
      return value === null;
    case "object":
      return isPlainObject(value);
    case "array":
      return Array.isArray(value);
    default:
      return false;
  }
}

async function runValidatedWorkflow<Output>(workflowId: string, inputSchema: JsonSchema, outputSchema: JsonSchema, input: unknown): Promise<Output> {
  validateContractValue(workflowId, "input", inputSchema, input);

  const workflow = await CodexWorkflow.start();
  try {
    const response = await workflow.workflows.run(workflowId, input);
    let output: unknown;
    try {
      output = JSON.parse(response.message);
    } catch (error) {
      failContract(workflowId, "output", "$", `failed to parse workflow output JSON: ${String(error)}`);
    }

    validateContractValue(workflowId, "output", outputSchema, output);
    return output as Output;
  } finally {
    await workflow.close();
  }
}
"##;

fn pascal_case(value: &str) -> String {
    let mut output = String::new();
    let mut capitalize_next = true;

    for ch in value.chars() {
        if matches!(ch, '_' | '-' | ' ' | '/' | '.') {
            capitalize_next = true;
            continue;
        }
        if capitalize_next {
            output.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            output.push(ch);
        }
    }

    output
}

fn json_literal(value: &JsonValue) -> Result<String> {
    serde_json::to_string(value).context("failed to serialize workflow schema literal")
}

fn relative_import_path(from_file: &Path, to_file: &Path) -> Result<String> {
    let from_dir = from_file
        .parent()
        .ok_or_else(|| anyhow!("generated workflow module has no parent"))?;

    let from_components = normalize_components(from_dir);
    let to_components = normalize_components(to_file);

    let mut common = 0;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }

    let mut parts = Vec::new();
    for _ in common..from_components.len() {
        parts.push("..".to_string());
    }
    for component in &to_components[common..] {
        parts.push(component.clone());
    }

    if parts.is_empty() {
        return Ok("./".to_string());
    }

    let mut path = parts.join("/");
    if !path.starts_with('.') {
        path = format!("./{path}");
    }
    Ok(path)
}

fn normalize_components(path: &Path) -> Vec<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let text = match component {
            std::path::Component::Prefix(prefix) => {
                prefix.as_os_str().to_string_lossy().to_string()
            }
            std::path::Component::RootDir => String::new(),
            std::path::Component::CurDir => continue,
            std::path::Component::ParentDir => "..".to_string(),
            std::path::Component::Normal(part) => part.to_string_lossy().to_string(),
        };
        if !text.is_empty() {
            components.push(text);
        }
    }
    components
}

fn render_ts_type(schema: &JsonValue) -> Result<String> {
    if let Some(any_of) = schema.get("anyOf").and_then(JsonValue::as_array) {
        let rendered = any_of
            .iter()
            .map(render_ts_type)
            .collect::<Result<Vec<_>>>()?;
        return Ok(render_union(rendered));
    }

    if let Some(enum_values) = schema.get("enum").and_then(JsonValue::as_array) {
        let mut rendered = enum_values
            .iter()
            .map(render_ts_literal)
            .collect::<Result<Vec<_>>>()?;

        if type_allows_null(schema) && !rendered.iter().any(|item| item == "null") {
            rendered.push("null".to_string());
        }

        return Ok(render_union(rendered));
    }

    let type_spec = schema.get("type");
    match type_spec {
        Some(JsonValue::String(type_name)) => render_single_type(schema, type_name),
        Some(JsonValue::Array(types)) => {
            let mut rendered = Vec::new();
            for type_name in types.iter().filter_map(JsonValue::as_str) {
                rendered.push(render_single_type(schema, type_name)?);
            }
            Ok(render_union(rendered))
        }
        _ => render_shape_type(schema),
    }
}

fn render_single_type(schema: &JsonValue, type_name: &str) -> Result<String> {
    Ok(match type_name {
        "string" => "string".to_string(),
        "number" => "number".to_string(),
        "integer" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => render_array_type(schema)?,
        "object" => render_object_type(schema)?,
        other => return Err(anyhow!("unsupported workflow schema type {other}")),
    })
}

fn render_shape_type(schema: &JsonValue) -> Result<String> {
    if schema.get("properties").is_some() || schema.get("additionalProperties").is_some() {
        return render_object_type(schema);
    }
    if schema.get("prefixItems").is_some() || schema.get("items").is_some() {
        return render_array_type(schema);
    }
    Ok("unknown".to_string())
}

fn render_object_type(schema: &JsonValue) -> Result<String> {
    let properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut fields = Vec::new();
    let mut names = properties.keys().cloned().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let Some(property_schema) = properties.get(&name) else {
            return Err(anyhow!("property `{name}` must exist in schema object"));
        };
        let optional = !required.contains(name.as_str());
        fields.push(format!(
            "{}{}: {}",
            render_property_name(&name),
            if optional { "?" } else { "" },
            render_ts_type(property_schema)?,
        ));
    }

    match schema.get("additionalProperties") {
        Some(JsonValue::Bool(true)) => fields.push("[key: string]: unknown".to_string()),
        Some(JsonValue::Object(additional_schema)) => fields.push(format!(
            "[key: string]: {}",
            render_ts_type(&JsonValue::Object(additional_schema.clone()))?,
        )),
        None | Some(JsonValue::Bool(false)) => {}
        Some(_) => return Err(anyhow!("unsupported workflow object schema")),
    }

    if fields.is_empty() {
        Ok("{}".to_string())
    } else {
        Ok(format!("{{ {} }}", fields.join("; ")))
    }
}

fn render_array_type(schema: &JsonValue) -> Result<String> {
    if let Some(prefix_items) = schema.get("prefixItems").and_then(JsonValue::as_array) {
        let mut items = Vec::new();
        for item_schema in prefix_items {
            items.push(render_ts_type(item_schema)?);
        }
        return Ok(format!("[{}]", items.join(", ")));
    }

    if let Some(item_schema) = schema.get("items") {
        let item_type = render_ts_type(item_schema)?;
        return Ok(format!("{}[]", wrap_union_type(&item_type)));
    }

    Ok("unknown[]".to_string())
}

fn render_union(mut items: Vec<String>) -> String {
    items.retain(|item| !item.is_empty());
    items.sort();
    items.dedup();
    match items.len() {
        0 => "unknown".to_string(),
        1 => items.pop().unwrap_or_else(|| "unknown".to_string()),
        _ => items.join(" | "),
    }
}

fn wrap_union_type(type_name: &str) -> String {
    if type_name.contains(" | ") {
        format!("({type_name})")
    } else {
        type_name.to_string()
    }
}

fn render_ts_literal(value: &JsonValue) -> Result<String> {
    Ok(match value {
        JsonValue::String(_) | JsonValue::Number(_) | JsonValue::Bool(_) | JsonValue::Null => {
            serde_json::to_string(value).context("failed to serialize literal schema value")?
        }
        _ => return Err(anyhow!("workflow literal schemas must be scalars")),
    })
}

fn render_property_name(name: &str) -> String {
    if is_valid_ts_identifier(name) {
        name.to_string()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
    }
}

fn is_valid_ts_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first == '$' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn type_allows_null(schema: &JsonValue) -> bool {
    match schema.get("type") {
        Some(JsonValue::String(type_name)) => type_name == "null",
        Some(JsonValue::Array(types)) => types
            .iter()
            .any(|type_name| type_name.as_str() == Some("null")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::GENERATED_WORKFLOW_MODULE_PATH;
    use super::generate_workflow_client_modules;
    use crate::api_contract::WorkflowSourceContract;
    use crate::registry::WorkflowRootKind;
    use crate::registry::WorkflowSummary;
    use crate::registry::WorkflowValidation;
    use crate::registry::WorkflowValidationStatus;

    fn workflow_summary(workflow_dir: &std::path::Path, id: &str) -> WorkflowSummary {
        WorkflowSummary {
            id: id.to_string(),
            command: Some(id.split('/').next_back().unwrap_or(id).to_string()),
            title: Some(id.to_string()),
            user_description: Some(id.to_string()),
            search_terms: Vec::new(),
            command_option_hints: Vec::new(),
            input_schema: None,
            root_label: "global".to_string(),
            root_kind: WorkflowRootKind::Global,
            root_path: workflow_dir.to_path_buf(),
            path: workflow_dir.to_path_buf(),
            workflow_yaml_path: workflow_dir.join("workflow.yaml"),
            mention_target: format!("workflow:///tmp#{id}"),
            validation: WorkflowValidation {
                status: WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        }
    }

    fn workflow_contract(callable_name: &str) -> WorkflowSourceContract {
        WorkflowSourceContract {
            callable_name: Some(callable_name.to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "workflowId": { "type": "string" },
                    "limit": { "type": ["integer", "null"], "minimum": 0, "maximum": 10 },
                    "areas": {
                        "type": ["array", "null"],
                        "items": { "type": "string", "enum": ["Code", "Test"] },
                        "minItems": 1
                    },
                    "includeSkipped": { "type": ["boolean", "null"], "enum": [false, true] }
                },
                "required": ["workflowId"],
                "additionalProperties": false
            }),
            output_schema: json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" }
                },
                "required": ["status"],
                "additionalProperties": false
            }),
            format_schemas: BTreeMap::from([(
                "tui.markdown.v1".to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "markdown": { "type": "string" }
                    },
                    "required": ["markdown"],
                    "additionalProperties": false
                }),
            )]),
            format_hook: true,
            hooks: Default::default(),
        }
    }

    #[test]
    fn generate_workflow_client_modules_renders_wrappers_and_formatter_exports() {
        let workflow_dir = TempDir::new().expect("workflow dir");
        fs::create_dir_all(workflow_dir.path().join("src")).expect("workflow src");
        fs::write(
            workflow_dir.path().join("workflow.yaml"),
            "id: review/code-review\n",
        )
        .expect("workflow yaml");

        let workflow = workflow_summary(workflow_dir.path(), "review/code-review");
        let mut source_contracts = BTreeMap::new();
        source_contracts.insert(workflow.path.clone(), workflow_contract("codeReview"));

        generate_workflow_client_modules(std::slice::from_ref(&workflow), &source_contracts)
            .expect("generate client module");

        let generated = fs::read_to_string(workflow.path.join(GENERATED_WORKFLOW_MODULE_PATH))
            .expect("generated module");

        assert!(
            generated.contains("import { CodexWorkflow } from \"@openai/codex-sdk/workflow\";")
        );
        assert!(generated.contains("import CodeReviewWorkflow from \"../workflow.js\";"));
        assert!(generated.contains("export type CodeReviewInput = {"));
        assert!(generated.contains("workflowId: string"));
        assert!(generated.contains("limit?: null | number"));
        assert!(generated.contains("areas?: (\"Code\" | \"Test\")[] | null"));
        assert!(generated.contains("includeSkipped?:"));
        assert!(generated.contains(" | null"));
        assert!(generated.contains("export type CodeReviewOutput = { status: string };"));
        assert!(generated.contains("export type JsonSchema = {"));
        assert!(generated.contains("minimum?: number;"));
        assert!(generated.contains("maximum?: number;"));
        assert!(generated.contains("export class WorkflowContractError extends Error"));
        assert!(generated.contains("export function validateContractValue("));
        assert!(generated.contains("export const codeReviewInputSchema = "));
        assert!(generated.contains("\"minimum\":0"));
        assert!(generated.contains("\"maximum\":10"));
        assert!(generated.contains(
            "export async function codeReview(input: CodeReviewInput): Promise<CodeReviewOutput> {"
        ));
        assert!(generated.contains("return runValidatedWorkflow<CodeReviewOutput>(\"review/code-review\", codeReviewInputSchema, codeReviewOutputSchema, input);"));
        assert!(
            generated.contains("validateContractValue(workflowId, \"input\", inputSchema, input);")
        );
        assert!(generated.contains("output = JSON.parse(response.message);"));
        assert!(generated.contains("export const CodeReviewOutput = {"));
        assert!(generated.contains(
            "toTuiMarkdown(result: CodeReviewOutput): { markdown: string } | Promise<{ markdown: string }> {"
        ));
        assert!(generated.contains("const formatter = (CodeReviewWorkflow as { format?:"));
        assert!(
            generated
                .contains("return formatter(result as never, { format: \"tui.markdown.v1\" });")
        );
        assert!(!generated.contains("CodeReviewWorkflowOutput.toTuiMarkdown(result as never);"));
        assert!(
            generated.contains(
                "validateNumberBounds(workflowId, direction, schema, value, schemaPath);"
            )
        );
        assert!(generated.contains("const maxItems = schema.maxItems ?? (schema.items ? Number.POSITIVE_INFINITY : prefixItems.length);"));
    }

    #[test]
    fn generate_workflow_client_modules_rejects_duplicate_callable_names() {
        let left_dir = TempDir::new().expect("left workflow dir");
        let right_dir = TempDir::new().expect("right workflow dir");
        fs::create_dir_all(left_dir.path().join("src")).expect("left src");
        fs::create_dir_all(right_dir.path().join("src")).expect("right src");

        let left = workflow_summary(left_dir.path(), "review/left");
        let right = workflow_summary(right_dir.path(), "review/right");

        let mut source_contracts = BTreeMap::new();
        source_contracts.insert(left.path.clone(), workflow_contract("sharedReview"));
        source_contracts.insert(right.path.clone(), workflow_contract("sharedReview"));

        let err = generate_workflow_client_modules(&[left, right], &source_contracts)
            .expect_err("duplicate callable names should fail");
        assert!(
            err.to_string()
                .contains("duplicate workflow callable name `sharedReview`")
        );
    }
}
