use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use sha2::Digest;
use sha2::Sha256;

use crate::registry::WorkflowSummary;

const WORKFLOW_API_CONTRACTS_DIR: &str = "workflow-api-contracts";
const WORKFLOW_API_EXTRACTOR_SOURCE: &str = r#"
import process from "node:process";
import path from "node:path";
import ts from "typescript";

const workflowPath = process.argv[1];

if (!workflowPath) {
  throw new Error("missing workflow path");
}

const normalizedWorkflowPath = path.resolve(workflowPath);
const program = ts.createProgram({
  rootNames: [normalizedWorkflowPath],
  options: {
    target: ts.ScriptTarget.ES2022,
    module: ts.ModuleKind.NodeNext,
    moduleResolution: ts.ModuleResolutionKind.NodeNext,
    noEmit: true,
    strict: true,
    allowJs: false,
    esModuleInterop: true,
    skipLibCheck: true,
  },
});

const diagnostics = ts
  .getPreEmitDiagnostics(program)
  .filter((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error);
if (diagnostics.length > 0) {
  const host = {
    getCanonicalFileName: (fileName) => fileName,
    getCurrentDirectory: () => process.cwd(),
    getNewLine: () => "\n",
  };
  throw new Error(ts.formatDiagnosticsWithColorAndContext(diagnostics, host));
}

const checker = program.getTypeChecker();
const sourceFile = program.getSourceFile(normalizedWorkflowPath);
if (!sourceFile) {
  throw new Error(`failed to load ${normalizedWorkflowPath}`);
}

const moduleSymbol = checker.getSymbolAtLocation(sourceFile);
if (!moduleSymbol) {
  throw new Error(`failed to resolve module symbol for ${normalizedWorkflowPath}`);
}

const moduleExports = new Map(
  checker
    .getExportsOfModule(moduleSymbol)
    .map((symbol) => [symbol.name, symbol]),
);

function isModifier(node, kind) {
  return ts.canHaveModifiers(node) && (ts.getModifiers(node) ?? []).some((modifier) => modifier.kind === kind);
}

function findNamedDefaultExportFunction(sourceFile) {
  for (const statement of sourceFile.statements) {
    if (!ts.isFunctionDeclaration(statement)) {
      continue;
    }
    if (!isModifier(statement, ts.SyntaxKind.ExportKeyword) || !isModifier(statement, ts.SyntaxKind.DefaultKeyword)) {
      continue;
    }
    if (!isModifier(statement, ts.SyntaxKind.AsyncKeyword)) {
      unsupportedType(checker.getTypeAtLocation(statement), "workflow default export must be async");
    }
    if (!statement.name) {
      throw new Error("workflow default export must be a named function");
    }
    if (statement.parameters.length < 2) {
      throw new Error("workflow default export must accept ctx and input parameters");
    }
    return statement;
  }
  return null;
}

function rejectAnonymousDefaultExportFunctions(sourceFile) {
  for (const statement of sourceFile.statements) {
    if (!ts.isExportAssignment(statement)) {
      continue;
    }
    const expression = statement.expression;
    if (ts.isArrowFunction(expression) || ts.isFunctionExpression(expression)) {
      throw new Error("workflow default export must be a named function");
    }
  }
}

function schemaFromDefaultExportFunction(declaration) {
  const inputType = checker.getTypeAtLocation(declaration.parameters[1]);
  const signature = checker.getSignatureFromDeclaration(declaration);
  if (!signature) {
    throw new Error(`failed to resolve default export signature for ${declaration.name.text}`);
  }
  const awaitedReturnType = checker.getAwaitedType(signature.getReturnType()) ?? signature.getReturnType();
  const outputSchema = schemaForType(awaitedReturnType);
  validateWorkflowOutputFormatter(outputSchema);
  return {
    callableName: declaration.name.text,
    inputSchema: schemaForType(inputType),
    outputSchema,
    hooks: {
      complete: hasModuleCompleteHook(),
    },
  };
}

function findDefaultExportExpression(sourceFile) {
  for (const statement of sourceFile.statements) {
    if (ts.isExportAssignment(statement)) {
      return statement.expression;
    }
  }
  return null;
}

function hasModuleCompleteHook() {
  const symbol = moduleExports.get("complete");
  if (!symbol || !symbol.valueDeclaration) {
    return false;
  }
  const completeType = checker.getTypeOfSymbolAtLocation(symbol, symbol.valueDeclaration);
  return typeHasCallSignature(completeType);
}

function typeHasCallSignature(type) {
  if (type.getCallSignatures().length > 0) {
    return true;
  }
  if (type.isUnion()) {
    return type.types
      .filter((member) =>
        (member.flags & ts.TypeFlags.Null) === 0
        && (member.flags & ts.TypeFlags.Undefined) === 0)
      .some(typeHasCallSignature);
  }
  return false;
}

function hasWorkflowCompleteHook(workflowType) {
  const complete = workflowType.getProperty("complete");
  if (!complete) {
    return false;
  }
  const declaration = complete.valueDeclaration ?? complete.declarations?.[0];
  if (!declaration) {
    return false;
  }
  const completeType = checker.getTypeOfSymbolAtLocation(complete, declaration);
  return typeHasCallSignature(completeType);
}

function stringLiteralPropertyFromObjectLiteral(expression, name) {
  if (!ts.isObjectLiteralExpression(expression)) {
    return null;
  }
  for (const property of expression.properties) {
    if (!ts.isPropertyAssignment(property)) {
      continue;
    }
    const propertyName = ts.isIdentifier(property.name) || ts.isStringLiteral(property.name)
      ? property.name.text
      : null;
    if (propertyName === name && ts.isStringLiteral(property.initializer)) {
      return property.initializer.text;
    }
  }
  return null;
}

function workflowObjectExpression(expression) {
  if (ts.isCallExpression(expression) && expression.arguments.length > 0) {
    return expression.arguments[0];
  }
  return expression;
}

function objectLiteralHasCallableProperty(expression, name) {
  if (!ts.isObjectLiteralExpression(expression)) {
    return false;
  }
  for (const property of expression.properties) {
    const propertyName = ts.isMethodDeclaration(property) || ts.isPropertyAssignment(property)
      ? (ts.isIdentifier(property.name) || ts.isStringLiteral(property.name) ? property.name.text : null)
      : null;
    if (propertyName !== name) {
      continue;
    }
    if (ts.isMethodDeclaration(property)) {
      return true;
    }
    if (ts.isPropertyAssignment(property)) {
      const propertyType = checker.getTypeAtLocation(property.initializer);
      return typeHasCallSignature(propertyType);
    }
  }
  return false;
}

function callableNameFromWorkflowExpression(expression, workflowType) {
  const objectExpression = workflowObjectExpression(expression);
  const literal = stringLiteralPropertyFromObjectLiteral(objectExpression, "callableName");
  if (literal) {
    return literal;
  }

  const property = workflowType.getProperty("callableName");
  if (!property) {
    return null;
  }
  const declaration = property.valueDeclaration ?? property.declarations?.[0];
  if (!declaration) {
    return null;
  }
  const propertyType = checker.getTypeOfSymbolAtLocation(property, declaration);
  return propertyType.isStringLiteral() ? propertyType.value : null;
}

function schemaFromWorkflowExpression(expression) {
  const objectExpression = workflowObjectExpression(expression);
  const workflowType = checker.getTypeAtLocation(expression);
  const run = workflowType.getProperty("run");
  if (!run) {
    return null;
  }
  const runDeclaration = run.valueDeclaration ?? run.declarations?.[0];
  if (!runDeclaration) {
    throw new Error("failed to inspect workflow run(ctx, input)");
  }
  const runType = checker.getTypeOfSymbolAtLocation(run, runDeclaration);
  const signatures = runType.getCallSignatures();
  if (signatures.length === 0) {
    unsupportedType(runType, "workflow run(ctx, input) must be callable");
  }
  const signature = signatures[0];
  if (signature.parameters.length < 2) {
    throw new Error("workflow run(ctx, input) must accept ctx and input parameters");
  }

  const inputSymbol = signature.parameters[1];
  const inputDeclaration = inputSymbol.valueDeclaration ?? inputSymbol.declarations?.[0];
  const inputType = checker.getTypeOfSymbolAtLocation(inputSymbol, inputDeclaration ?? runDeclaration);
  const awaitedReturnType = checker.getAwaitedType(signature.getReturnType()) ?? signature.getReturnType();
  const outputSchema = schemaForType(awaitedReturnType);
  const formatSchemas = formatSchemasFromWorkflowExpression(workflowType);
  return {
    callableName: callableNameFromWorkflowExpression(expression, workflowType),
    inputSchema: schemaForType(inputType),
    outputSchema,
    formatSchemas,
    formatHook: Object.keys(formatSchemas).length > 0,
    hooks: {
      complete: objectLiteralHasCallableProperty(objectExpression, "complete")
        || hasWorkflowCompleteHook(workflowType)
        || hasModuleCompleteHook(),
    },
  };
}

function formatSchemasFromWorkflowExpression(workflowType) {
  const format = workflowType.getProperty("format");
  if (!format) {
    return {};
  }

  const declaration = format.valueDeclaration ?? format.declarations?.[0];
  if (!declaration) {
    throw new Error("failed to inspect workflow format(result, request)");
  }

  const formatType = checker.getTypeOfSymbolAtLocation(format, declaration);
  const signatures = callableSignatures(formatType);
  if (signatures.length === 0) {
    unsupportedType(formatType, "workflow format(result, request) must be callable");
  }

  const formats = {};
  for (const signature of signatures) {
    if (signature.parameters.length < 2) {
      throw new Error("workflow format(result, request) must accept result and request parameters");
    }

    const requestSymbol = signature.parameters[1];
    const requestDeclaration = requestSymbol.valueDeclaration ?? requestSymbol.declarations?.[0] ?? declaration;
    const requestType = checker.getTypeOfSymbolAtLocation(requestSymbol, requestDeclaration);
    const formatNames = requestFormatNames(requestType);
    if (formatNames.length === 0) {
      continue;
    }

    const returnType = checker.getAwaitedType(signature.getReturnType()) ?? signature.getReturnType();
    const returnSchema = schemaForType(returnType);
    for (const formatName of formatNames) {
      validateFormatSchema(formatName, returnSchema, "workflow format(result, request)");
      formats[formatName] = returnSchema;
    }
  }

  return formats;
}

function callableSignatures(type) {
  const signatures = type.getCallSignatures();
  if (signatures.length > 0) {
    return signatures;
  }

  if (type.isUnion()) {
    return type.types.flatMap((candidate) => {
      if ((candidate.flags & ts.TypeFlags.Undefined) !== 0) {
        return [];
      }
      return callableSignatures(candidate);
    });
  }

  return [];
}

function requestFormatNames(requestType) {
  const formatProperty = requestType.getProperty("format");
  if (!formatProperty) {
    return [];
  }

  const declaration = formatProperty.valueDeclaration ?? formatProperty.declarations?.[0];
  const formatType = checker.getTypeOfSymbolAtLocation(formatProperty, declaration ?? sourceFile);
  return stringLiteralValues(formatType);
}

function stringLiteralValues(type) {
  if (type.isStringLiteral()) {
    return [type.value];
  }
  if (type.isUnion()) {
    return type.types.flatMap(stringLiteralValues);
  }
  if (type.isIntersection()) {
    for (const candidate of type.types) {
      const values = stringLiteralValues(candidate);
      if (values.length > 0) {
        return values;
      }
    }
  }
  return [];
}

function validateFormatSchema(formatName, schema, label) {
  if (formatName !== "tui.markdown.v1") {
    return;
  }

  const properties = schema.properties ?? {};
  if (schema.type !== "object" || !properties.markdown || properties.markdown.type !== "string") {
    throw new Error(`${label} for tui.markdown.v1 must return { markdown: string }`);
  }
}

function validateWorkflowOutputFormatter(outputSchema) {
  const symbol = moduleExports.get("WorkflowOutput");
  if (!symbol || !symbol.valueDeclaration) {
    return;
  }

  const declaredType = checker.getTypeOfSymbolAtLocation(symbol, symbol.valueDeclaration);
  const formatter = declaredType.getProperty("toTuiMarkdown");
  if (!formatter) {
    throw new Error("WorkflowOutput value export must define toTuiMarkdown(result)");
  }

  const declaration = formatter.valueDeclaration ?? formatter.declarations?.[0];
  if (!declaration) {
    throw new Error("failed to inspect WorkflowOutput.toTuiMarkdown(result)");
  }

  const formatterType = checker.getTypeOfSymbolAtLocation(formatter, declaration);
  const signatures = formatterType.getCallSignatures();
  if (signatures.length === 0) {
    unsupportedType(formatterType, "WorkflowOutput.toTuiMarkdown(result) must be callable");
  }

  const signature = signatures[0];
  if (signature.parameters.length === 0) {
    throw new Error("WorkflowOutput.toTuiMarkdown(result) must accept a result parameter");
  }

  const returnType = checker.getAwaitedType(signature.getReturnType()) ?? signature.getReturnType();
  const returnSchema = schemaForType(returnType);
  const properties = returnSchema.properties ?? {};
  if (returnSchema.type !== "object" || !properties.markdown || properties.markdown.type !== "string") {
    throw new Error("WorkflowOutput.toTuiMarkdown(result) must return { markdown: string }");
  }
}

function unsupportedType(type, reason) {
  const label = checker.typeToString(type);
  throw new Error(`${reason}: ${label}`);
}

function symbolDescription(symbol) {
  const description = ts.displayPartsToString(symbol.getDocumentationComment(checker)).trim();
  return description.length > 0 ? description : undefined;
}

function symbolTagText(symbol, tagName) {
  const tag = symbol.getJsDocTags(checker).find((candidate) => candidate.name === tagName);
  if (!tag?.text) {
    return undefined;
  }
  return tag.text.map((part) => part.text).join("").trim();
}

function applySymbolSchemaTags(symbol, schema) {
  if (symbol.getJsDocTags(checker).some((tag) => tag.name === "integer")) {
    if (schema.type === "number") {
      schema.type = "integer";
    } else if (Array.isArray(schema.type)) {
      schema.type = schema.type.map((typeName) => typeName === "number" ? "integer" : typeName);
    }
  }

  for (const [tagName, schemaName] of [
    ["minimum", "minimum"],
    ["maximum", "maximum"],
  ]) {
    const value = symbolTagText(symbol, tagName);
    if (value !== undefined) {
      const numberValue = Number(value);
      if (!Number.isFinite(numberValue)) {
        throw new Error(`WorkflowInput.${symbol.name} @${tagName} must be a finite number`);
      }
      schema[schemaName] = numberValue;
    }
  }

  const minItems = symbolTagText(symbol, "minItems");
  if (minItems !== undefined) {
    const minItemsValue = Number(minItems);
    if (!Number.isInteger(minItemsValue) || minItemsValue < 0) {
      throw new Error(`WorkflowInput.${symbol.name} @minItems must be a non-negative integer`);
    }
    schema.minItems = minItemsValue;
  }
}

function schemaForType(type, stack = new Set()) {
  if ((type.flags & ts.TypeFlags.Any) !== 0) {
    unsupportedType(type, "workflow API types cannot use any");
  }
  if ((type.flags & ts.TypeFlags.Unknown) !== 0) {
    unsupportedType(type, "workflow API types cannot use unknown");
  }
  if ((type.flags & ts.TypeFlags.Never) !== 0) {
    unsupportedType(type, "workflow API types cannot use never");
  }
  if ((type.flags & ts.TypeFlags.Void) !== 0) {
    unsupportedType(type, "workflow API types cannot use void");
  }

  if (type.getCallSignatures().length > 0 || type.getConstructSignatures().length > 0) {
    unsupportedType(type, "function-valued workflow API types are not supported");
  }

  if (type.isStringLiteral()) {
    return { type: "string", enum: [type.value] };
  }
  if (type.isNumberLiteral()) {
    return { type: "number", enum: [type.value] };
  }
  if ((type.flags & ts.TypeFlags.BooleanLiteral) !== 0) {
    const intrinsicName = type.intrinsicName;
    return { type: "boolean", enum: [intrinsicName === "true"] };
  }
  if ((type.flags & ts.TypeFlags.StringLike) !== 0) {
    return { type: "string" };
  }
  if ((type.flags & ts.TypeFlags.NumberLike) !== 0) {
    return { type: "number" };
  }
  if ((type.flags & ts.TypeFlags.BooleanLike) !== 0) {
    return { type: "boolean" };
  }
  if ((type.flags & ts.TypeFlags.BigIntLike) !== 0) {
    return { type: "integer" };
  }
  if ((type.flags & ts.TypeFlags.Null) !== 0) {
    return { type: "null" };
  }

  if (type.isUnion()) {
    const members = type.types;
    const nonNullableMembers = members.filter(
      (member) =>
        (member.flags & ts.TypeFlags.Null) === 0
        && (member.flags & ts.TypeFlags.Undefined) === 0,
    );
    const hasNullish = nonNullableMembers.length !== members.length;
    const stringEnum = nonNullableMembers.every((member) => member.isStringLiteral());
    if (stringEnum) {
      return {
        type: hasNullish ? ["string", "null"] : "string",
        enum: nonNullableMembers.map((member) => member.value),
      };
    }

    const booleanEnum = nonNullableMembers.every(
      (member) => (member.flags & ts.TypeFlags.BooleanLiteral) !== 0,
    );
    if (booleanEnum) {
      return {
        type: hasNullish ? ["boolean", "null"] : "boolean",
        enum: nonNullableMembers.map((member) => member.intrinsicName === "true"),
      };
    }

    if (nonNullableMembers.length === 1 && hasNullish) {
      const inner = schemaForType(nonNullableMembers[0], stack);
      if (typeof inner.type === "string") {
        return { ...inner, type: [inner.type, "null"] };
      }
      return { anyOf: [inner, { type: "null" }] };
    }

    return { anyOf: nonNullableMembers.map((member) => schemaForType(member, stack)) };
  }

  if (checker.isArrayType(type)) {
    const [itemType] = checker.getTypeArguments(type);
    return {
      type: "array",
      items: itemType ? schemaForType(itemType, stack) : {},
    };
  }

  if (checker.isTupleType(type)) {
    const items = checker
      .getTypeArguments(type)
      .map((itemType) => schemaForType(itemType, stack));
    return {
      type: "array",
      prefixItems: items,
      minItems: items.length,
      maxItems: items.length,
    };
  }

  const symbol = type.getSymbol() ?? type.aliasSymbol;
  if (symbol) {
    if (stack.has(symbol)) {
      unsupportedType(type, "recursive workflow API types are not supported");
    }
    stack.add(symbol);
  }

  try {
    const properties = type.getProperties();
    if (properties.length > 0 || checker.getIndexTypeOfType(type, ts.IndexKind.String)) {
      const schemaProperties = {};
      const required = [];

      for (const property of properties) {
        const declaration = property.valueDeclaration ?? property.declarations?.[0];
        if (!declaration) {
          continue;
        }

        const propertyType = checker.getTypeOfSymbolAtLocation(property, declaration);
        const propertySchema = schemaForType(propertyType, stack);
        const description = symbolDescription(property);
        if (description) {
          propertySchema.description = description;
        }
        applySymbolSchemaTags(property, propertySchema);
        schemaProperties[property.name] = propertySchema;
        if ((property.flags & ts.SymbolFlags.Optional) === 0) {
          required.push(property.name);
        }
      }

      const indexType = checker.getIndexTypeOfType(type, ts.IndexKind.String);
      const schema = {
        type: "object",
        properties: schemaProperties,
      };
      if (required.length > 0) {
        schema.required = required;
      }
      if (indexType) {
        schema.additionalProperties = schemaForType(indexType, stack);
      } else {
        schema.additionalProperties = false;
      }
      return schema;
    }
  } finally {
    if (symbol) {
      stack.delete(symbol);
    }
  }

  unsupportedType(type, "unsupported workflow API type");
}

function exportedTypeSchema(name) {
  const symbol = moduleExports.get(name);
  if (!symbol) {
    return null;
  }
  const declaredType = checker.getDeclaredTypeOfSymbol(symbol);
  return schemaForType(declaredType);
}

function formatSchemas(useTuiFormatter) {
  if (useTuiFormatter) {
    const symbol = moduleExports.get("WorkflowOutput");
    if (symbol && symbol.valueDeclaration) {
      const declaredType = checker.getTypeOfSymbolAtLocation(symbol, symbol.valueDeclaration);
      const formatter = declaredType.getProperty("toTuiMarkdown");
      if (formatter) {
        const declaration = formatter.valueDeclaration ?? formatter.declarations?.[0];
        if (!declaration) {
          throw new Error("failed to inspect WorkflowOutput.toTuiMarkdown(result)");
        }

        const formatterType = checker.getTypeOfSymbolAtLocation(formatter, declaration);
        const signatures = formatterType.getCallSignatures();
        if (signatures.length === 0) {
          unsupportedType(formatterType, "WorkflowOutput.toTuiMarkdown(result) must be callable");
        }

        const signature = signatures[0];
        const returnType = checker.getAwaitedType(signature.getReturnType()) ?? signature.getReturnType();
        const returnSchema = schemaForType(returnType);
        validateFormatSchema("tui.markdown.v1", returnSchema, "WorkflowOutput.toTuiMarkdown(result)");
        return {
          "tui.markdown.v1": returnSchema,
        };
      }
    }
  }

  const symbol = moduleExports.get("WorkflowFormats");
  if (!symbol) {
    return {};
  }
  const declaredType = checker.getDeclaredTypeOfSymbol(symbol);
  const properties = declaredType.getProperties();
  const formats = {};
  for (const property of properties) {
    const declaration = property.valueDeclaration ?? property.declarations?.[0];
    if (!declaration) {
      continue;
    }
    const propertyType = checker.getTypeOfSymbolAtLocation(property, declaration);
    formats[property.name] = schemaForType(propertyType);
  }
  return formats;
}

rejectAnonymousDefaultExportFunctions(sourceFile);

const defaultExportDeclaration = findNamedDefaultExportFunction(sourceFile);
const defaultExportContract = defaultExportDeclaration
  ? schemaFromDefaultExportFunction(defaultExportDeclaration)
  : (findDefaultExportExpression(sourceFile)
    ? schemaFromWorkflowExpression(findDefaultExportExpression(sourceFile))
    : null);

const legacyInputSchema = exportedTypeSchema("WorkflowInput");
const legacyOutputSchema = exportedTypeSchema("WorkflowOutput");
const inputSchema = defaultExportContract?.inputSchema ?? legacyInputSchema;
const outputSchema = defaultExportContract?.outputSchema ?? legacyOutputSchema;

if (!inputSchema || !outputSchema) {
  throw new Error(
    "workflow must export a named default function or define WorkflowInput and WorkflowOutput types",
  );
}

process.stdout.write(
  JSON.stringify({
    callableName: defaultExportContract?.callableName ?? null,
    inputSchema,
	    outputSchema,
	    formatSchemas: defaultExportContract?.formatSchemas ?? formatSchemas(Boolean(defaultExportContract)),
	    formatHook: defaultExportContract?.formatHook ?? false,
	    hooks: defaultExportContract?.hooks ?? { complete: hasModuleCompleteHook() },
	  }),
	);
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSourceContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callable_name: Option<String>,
    pub input_schema: JsonValue,
    pub output_schema: JsonValue,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub format_schemas: BTreeMap<String, JsonValue>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub format_hook: bool,
    #[serde(default, skip_serializing_if = "WorkflowContractHooks::is_empty")]
    pub hooks: WorkflowContractHooks,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowApiContract {
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub input_schema: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    pub output_schema: JsonValue,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub format_schemas: BTreeMap<String, JsonValue>,
    #[serde(default, skip_serializing_if = "WorkflowContractHooks::is_empty")]
    pub hooks: WorkflowContractHooks,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowContractHooks {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
}

impl WorkflowContractHooks {
    fn is_empty(&self) -> bool {
        !self.complete
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowApiContractRecord {
    workflow_id: String,
    workflow_path: PathBuf,
    source_digest: String,
    updated_at_unix_sec: u64,
    #[serde(default)]
    source_contract: Option<WorkflowSourceContract>,
    contract: WorkflowApiContract,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ExtractedWorkflowApiContract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    callable_name: Option<String>,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    input_schema: JsonValue,
    #[serde(default, skip_serializing_if = "JsonValue::is_null")]
    output_schema: JsonValue,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    format_schemas: BTreeMap<String, JsonValue>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    format_hook: bool,
    #[serde(default, skip_serializing_if = "WorkflowContractHooks::is_empty")]
    hooks: WorkflowContractHooks,
}

impl From<ExtractedWorkflowApiContract> for WorkflowSourceContract {
    fn from(value: ExtractedWorkflowApiContract) -> Self {
        Self {
            callable_name: value.callable_name,
            input_schema: value.input_schema,
            output_schema: value.output_schema,
            format_schemas: value.format_schemas,
            format_hook: value.format_hook,
            hooks: value.hooks,
        }
    }
}

impl From<WorkflowSourceContract> for WorkflowApiContract {
    fn from(value: WorkflowSourceContract) -> Self {
        Self {
            input_schema: value.input_schema,
            output_schema: value.output_schema,
            format_schemas: value.format_schemas,
            hooks: value.hooks,
        }
    }
}

pub(crate) fn read_published_workflow_api_contract(
    codex_home: &Path,
    workflow: &WorkflowSummary,
) -> Result<Option<WorkflowApiContract>> {
    let record_path = workflow_api_contract_record_path(codex_home, &workflow.path);
    let Ok(contents) = fs::read_to_string(&record_path) else {
        return Ok(None);
    };
    let record =
        serde_json::from_str::<WorkflowApiContractRecord>(&contents).with_context(|| {
            format!(
                "failed to parse workflow API contract {}",
                record_path.display()
            )
        })?;
    if record.workflow_id != workflow.id || record.workflow_path != workflow.path {
        return Ok(None);
    }
    Ok(Some(record.contract))
}

pub(crate) fn read_published_workflow_source_contract(
    codex_home: &Path,
    workflow: &WorkflowSummary,
) -> Result<Option<WorkflowSourceContract>> {
    let record_path = workflow_api_contract_record_path(codex_home, &workflow.path);
    let Ok(contents) = fs::read_to_string(&record_path) else {
        return Ok(None);
    };
    let record =
        serde_json::from_str::<WorkflowApiContractRecord>(&contents).with_context(|| {
            format!(
                "failed to parse workflow API contract {}",
                record_path.display()
            )
        })?;
    if record.workflow_id != workflow.id || record.workflow_path != workflow.path {
        return Ok(None);
    }
    Ok(Some(record.source_contract.unwrap_or({
        WorkflowSourceContract {
            callable_name: None,
            input_schema: record.contract.input_schema,
            output_schema: record.contract.output_schema,
            format_schemas: record.contract.format_schemas,
            format_hook: false,
            hooks: record.contract.hooks,
        }
    })))
}

pub(crate) fn publish_validated_workflow_api_contract(
    codex_home: &Path,
    workflow: &WorkflowSummary,
    source_contract: WorkflowSourceContract,
) -> Result<()> {
    let record_path = workflow_api_contract_record_path(codex_home, &workflow.path);
    let source_digest = workflow_source_digest(&workflow.path)?;
    let contract = WorkflowApiContract::from(source_contract.clone());
    if let Ok(contents) = fs::read_to_string(&record_path)
        && let Ok(existing) = serde_json::from_str::<WorkflowApiContractRecord>(&contents)
        && existing.workflow_id == workflow.id
        && existing.workflow_path == workflow.path
        && existing.source_digest == source_digest
        && existing.source_contract == Some(source_contract.clone())
        && existing.contract == contract
    {
        return Ok(());
    }

    let record = WorkflowApiContractRecord {
        workflow_id: workflow.id.clone(),
        workflow_path: workflow.path.clone(),
        source_digest,
        updated_at_unix_sec: unix_now(),
        source_contract: Some(source_contract),
        contract,
    };
    let record_dir = record_path
        .parent()
        .ok_or_else(|| anyhow!("workflow API contract path has no parent"))?;
    fs::create_dir_all(record_dir).with_context(|| {
        format!(
            "failed to create workflow API contract directory {}",
            record_dir.display()
        )
    })?;
    let temp_path = record_path.with_extension("json.tmp");
    fs::write(&temp_path, serde_json::to_vec_pretty(&record)?).with_context(|| {
        format!(
            "failed to write workflow API contract {}",
            temp_path.display()
        )
    })?;
    fs::rename(&temp_path, &record_path).with_context(|| {
        format!(
            "failed to publish workflow API contract {}",
            record_path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn extract_workflow_source_contract_from_typescript(
    workflow_dir: &Path,
) -> Result<WorkflowSourceContract> {
    ensure_repo_typescript_shim(workflow_dir)?;
    let workflow_path = workflow_dir.join("src/workflow.ts");
    let command =
        if let Some(managed_bun) = crate::managed_bun::ensure_managed_bun(/*cache_root*/ None)? {
            managed_bun
        } else {
            return Err(anyhow!(
                "workflow API extraction requires managed Bun in CODEX_HOME/workflows/.bin"
            ));
        };
    let output = run_workflow_api_extractor(command, workflow_dir, &workflow_path)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("workflow API extractor exited with {}", output.status)
        };
        return Err(anyhow!(detail));
    }
    let extracted = serde_json::from_slice::<ExtractedWorkflowApiContract>(&output.stdout)
        .with_context(|| {
            format!(
                "failed to decode workflow API contract from {}",
                workflow_path.display()
            )
        })?;
    Ok(extracted.into())
}

fn run_workflow_api_extractor(
    command: PathBuf,
    workflow_dir: &Path,
    workflow_path: &Path,
) -> Result<Output> {
    run_workflow_api_extractor_once(command, workflow_dir, workflow_path).with_context(|| {
        format!(
            "failed to extract workflow API from {}",
            workflow_path.display()
        )
    })
}

fn run_workflow_api_extractor_once(
    command: PathBuf,
    workflow_dir: &Path,
    workflow_path: &Path,
) -> std::io::Result<Output> {
    let mut command = Command::new(command);
    crate::managed_bun::configure_isolated_bun_environment(&mut command, /*cache_root*/ None)
        .map_err(std::io::Error::other)?;
    command
        .current_dir(workflow_dir)
        .args([
            "--eval",
            WORKFLOW_API_EXTRACTOR_SOURCE,
            "--",
            &workflow_path.display().to_string(),
        ])
        .output()
}

pub(crate) fn ensure_repo_typescript_shim(workflow_dir: &Path) -> Result<bool> {
    let typescript_dir = workflow_dir.join("node_modules/typescript");
    let typescript_package = typescript_dir.join("package.json");
    if typescript_package.is_file() {
        return Ok(true);
    }

    let Some(typescript_library) = repo_typescript_library() else {
        return Ok(false);
    };

    fs::create_dir_all(&typescript_dir).with_context(|| {
        format!(
            "failed to create TypeScript shim directory {}",
            typescript_dir.display()
        )
    })?;
    fs::write(
        typescript_dir.join("index.js"),
        format!(
            "module.exports = require({});\n",
            serde_json::to_string(typescript_library.to_string_lossy().as_ref())?
        ),
    )
    .with_context(|| {
        format!(
            "failed to write TypeScript shim {}",
            typescript_dir.join("index.js").display()
        )
    })?;
    fs::write(
        &typescript_package,
        "{\n  \"name\": \"typescript\",\n  \"private\": true,\n  \"main\": \"./index.js\"\n}\n",
    )
    .with_context(|| {
        format!(
            "failed to write TypeScript shim package {}",
            typescript_package.display()
        )
    })?;
    Ok(true)
}

fn repo_typescript_library() -> Option<PathBuf> {
    let relative_path = "node_modules/typescript/lib/typescript.js";
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let current_dir = std::env::current_dir().ok();
    let candidates = [
        bazel_runfile(relative_path),
        Some(manifest_dir.join("../..").join(relative_path)),
        current_dir
            .as_ref()
            .map(|cwd| cwd.join("..").join(relative_path)),
        current_dir.as_ref().map(|cwd| cwd.join(relative_path)),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|path| path.canonicalize().ok())
}

fn bazel_runfile(relative_path: &str) -> Option<PathBuf> {
    let runfile_path = format!("_main/{relative_path}");
    let runfile_suffix = format!("/{relative_path}");
    if let Some(manifest_file) = std::env::var_os("RUNFILES_MANIFEST_FILE")
        && let Ok(manifest) = fs::read_to_string(manifest_file)
    {
        for line in manifest.lines() {
            if let Some((logical_path, physical_path)) = line.split_once(' ')
                && (logical_path == runfile_path || logical_path.ends_with(&runfile_suffix))
            {
                return Some(PathBuf::from(physical_path));
            }
        }
    }

    let runfile_paths = [
        PathBuf::from(&runfile_path),
        PathBuf::from("__main__").join(relative_path),
        PathBuf::from(relative_path),
    ];
    ["RUNFILES_DIR", "TEST_SRCDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .flat_map(|root| {
            runfile_paths
                .iter()
                .map(move |path| PathBuf::from(&root).join(path))
        })
        .find(|path| path.is_file())
}

pub(crate) fn workflow_api_contract_from_spec_api(api: &JsonValue) -> Option<WorkflowApiContract> {
    if api.is_null() {
        return None;
    }
    Some(WorkflowApiContract {
        input_schema: api.get("inputSchema").cloned().unwrap_or(JsonValue::Null),
        output_schema: api.get("outputSchema").cloned().unwrap_or(JsonValue::Null),
        format_schemas: api
            .get("formatSchemas")
            .and_then(JsonValue::as_object)
            .map(|schemas| {
                schemas
                    .iter()
                    .map(|(name, schema)| (name.clone(), schema.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default(),
        hooks: WorkflowContractHooks::default(),
    })
}

fn workflow_api_contract_record_path(codex_home: &Path, workflow_path: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(workflow_path.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    codex_home
        .join(WORKFLOW_API_CONTRACTS_DIR)
        .join(format!("{digest}.json"))
}

fn workflow_source_digest(workflow_dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    for relative in [
        Path::new("workflow.yaml"),
        Path::new("package.json"),
        Path::new("tsconfig.json"),
        Path::new("src"),
    ] {
        collect_workflow_source_paths(workflow_dir, relative, &mut files)?;
    }
    files.sort();

    let mut hasher = Sha256::new();
    for file_path in files {
        let relative = file_path
            .strip_prefix(workflow_dir)
            .unwrap_or(file_path.as_path());
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(
            fs::read(&file_path).with_context(|| {
                format!("failed to read workflow source {}", file_path.display())
            })?,
        );
        hasher.update(b"\0");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_workflow_source_paths(
    workflow_dir: &Path,
    relative: &Path,
    paths: &mut Vec<PathBuf>,
) -> Result<()> {
    let path = workflow_dir.join(relative);
    if !path.exists() {
        return Ok(());
    }
    if path.is_file() {
        paths.push(path);
        return Ok(());
    }
    let mut entries = fs::read_dir(&path)
        .with_context(|| {
            format!(
                "failed to read workflow source directory {}",
                path.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_workflow_source_paths(
                workflow_dir,
                entry_path.strip_prefix(workflow_dir)?,
                paths,
            )?;
        } else if entry_path.is_file() {
            paths.push(entry_path);
        }
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
pub(crate) fn prepare_typescript_workflow_dir(workflow_dir: &Path) -> bool {
    fs::create_dir_all(workflow_dir.join("src")).expect("workflow src");
    fs::create_dir_all(workflow_dir.join("node_modules/typescript"))
        .expect("workflow node_modules");
    fs::write(
        workflow_dir.join("package.json"),
        r#"{
  "name": "workflow-test",
  "private": true,
  "type": "module"
}
"#,
    )
    .expect("package json");
    fs::write(
        workflow_dir.join("tsconfig.json"),
        r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext"
  }
}
"#,
    )
    .expect("tsconfig");

    if !ensure_repo_typescript_shim(workflow_dir).expect("typescript shim") {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;

    use super::WorkflowApiContract;
    use super::WorkflowSourceContract;
    use super::extract_workflow_source_contract_from_typescript;
    use super::publish_validated_workflow_api_contract;
    use super::read_published_workflow_api_contract;
    use super::workflow_api_contract_from_spec_api;
    use crate::registry::WorkflowEngine;
    use crate::registry::WorkflowRootKind;
    use crate::registry::WorkflowSummary;
    use crate::registry::WorkflowValidation;
    use crate::registry::WorkflowValidationStatus;

    fn write_workflow_source(workflow_dir: &Path, source: &str) -> bool {
        if !super::prepare_typescript_workflow_dir(workflow_dir) {
            return false;
        }
        fs::write(workflow_dir.join("src/workflow.ts"), source).expect("workflow ts");
        true
    }

    #[test]
    fn workflow_api_contract_from_spec_reads_input_output_and_formats() {
        let contract = workflow_api_contract_from_spec_api(&json!({
            "inputSchema": { "type": "object" },
            "outputSchema": { "type": "object", "properties": { "ok": { "type": "boolean" } } },
            "formatSchemas": {
                "tui.markdown.v1": { "type": "object", "properties": { "markdown": { "type": "string" } } }
            }
        }))
        .expect("contract from spec");

        assert_eq!(contract.input_schema, json!({ "type": "object" }));
        assert_eq!(
            contract.output_schema["properties"]["ok"]["type"],
            json!("boolean")
        );
        assert_eq!(
            contract.format_schemas["tui.markdown.v1"]["properties"]["markdown"]["type"],
            json!("string")
        );
    }

    #[test]
    fn publish_and_read_workflow_api_contract_round_trip() {
        let codex_home = TempDir::new().expect("codex home");
        let workflow_root = TempDir::new().expect("workflow root");
        let workflow_dir = workflow_root.path().join("review/fix");
        std::fs::create_dir_all(workflow_dir.join("src")).expect("create workflow src");
        std::fs::write(workflow_dir.join("workflow.yaml"), "id: review/fix\n")
            .expect("workflow yaml");
        std::fs::write(workflow_dir.join("package.json"), "{}\n").expect("package json");
        std::fs::write(workflow_dir.join("tsconfig.json"), "{}\n").expect("tsconfig");
        std::fs::write(workflow_dir.join("src/workflow.ts"), "export {}\n").expect("workflow ts");

        let workflow = WorkflowSummary {
            id: "review/fix".to_string(),
            engine: WorkflowEngine::TypeScript,
            command: Some("fix".to_string()),
            title: Some("Fix".to_string()),
            user_description: Some("Fix workflow".to_string()),
            search_terms: Vec::new(),
            command_option_hints: Vec::new(),
            input_schema: None,
            root_label: "global".to_string(),
            root_kind: WorkflowRootKind::Global,
            root_path: workflow_root.path().to_path_buf(),
            path: workflow_dir.clone(),
            workflow_yaml_path: workflow_dir.join("workflow.yaml"),
            mention_target: "workflow:///tmp#review/fix".to_string(),
            validation: WorkflowValidation {
                status: WorkflowValidationStatus::Valid,
                findings: Vec::new(),
            },
            repair_mode: "full".to_string(),
        };
        let source_contract = WorkflowSourceContract {
            callable_name: Some("fixReview".to_string()),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            format_schemas: BTreeMap::from([(
                "tui.markdown.v1".to_string(),
                json!({ "type": "object", "properties": { "markdown": { "type": "string" } } }),
            )]),
            format_hook: false,
            hooks: Default::default(),
        };
        let contract = WorkflowApiContract::from(source_contract.clone());

        publish_validated_workflow_api_contract(codex_home.path(), &workflow, source_contract)
            .expect("publish contract");

        let published = read_published_workflow_api_contract(codex_home.path(), &workflow)
            .expect("read contract")
            .expect("missing contract");
        assert_eq!(published, contract);
    }

    #[test]
    fn extract_workflow_source_contract_from_typescript_extracts_named_default_export_and_formatter()
     {
        let workflow_dir = TempDir::new().expect("workflow dir");
        if !write_workflow_source(
            workflow_dir.path(),
            r#"
export interface WorkflowInput {
  workflowId: string;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.status };
  },
};

export default async function codeReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.workflowId };
}
"#,
        ) {
            return;
        }

        let contract = extract_workflow_source_contract_from_typescript(workflow_dir.path())
            .expect("workflow source contract");

        assert_eq!(contract.callable_name.as_deref(), Some("codeReview"));
        assert_eq!(
            contract.input_schema,
            json!({
                "type": "object",
                "properties": {
                    "workflowId": { "type": "string" }
                },
                "required": ["workflowId"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            contract.output_schema,
            json!({
                "type": "object",
                "properties": {
                    "status": { "type": "string" }
                },
                "required": ["status"],
                "additionalProperties": false
            })
        );
        assert_eq!(
            contract.format_schemas,
            BTreeMap::from([(
                "tui.markdown.v1".to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "markdown": { "type": "string" }
                    },
                    "required": ["markdown"],
                    "additionalProperties": false
                })
            )])
        );
        assert!(!contract.hooks.complete);
    }

    #[test]
    fn extract_workflow_source_contract_from_typescript_extracts_define_workflow_contract_and_hooks()
     {
        let workflow_dir = TempDir::new().expect("workflow dir");
        if !write_workflow_source(
            workflow_dir.path(),
            r#"
	type WorkflowContext = unknown;
	type WorkflowCompletionRequest<Input> = {
	  input: Partial<Input>;
	  activeField?: keyof Input & string;
	  prefix: string;
	  mode: "field" | "value";
	};
	type WorkflowCompletionSuggestion<Input> =
	  | { type: "field"; field: keyof Input & string }
	  | { type: "value"; value: string };
		type DefinedWorkflow<Input, Output> = {
		  callableName?: string;
		  run(ctx: WorkflowContext, input: Input): Promise<Output>;
		  complete?(ctx: WorkflowContext, request: WorkflowCompletionRequest<Input>): Promise<WorkflowCompletionSuggestion<Input>[]>;
		  format?(result: Output, request: { format: "tui.markdown.v1" }): { markdown: string };
		};
	declare function defineWorkflow<Input, Output>(workflow: DefinedWorkflow<Input, Output>): DefinedWorkflow<Input, Output>;

	export type WorkflowInput = {
	  /** Review identifier */
	  reviewId: string;
	  /** Review areas to include
	   * @minItems 1
	   */
	  allowedAreas?: string[];
	  /** Output format */
	  format: "json" | "markdown";
	  /** Maximum number of results
	   * @integer
	   * @minimum 0
	   * @maximum 10
	   */
	  limit?: number;
	  /** Include skipped findings */
	  includeSkipped?: boolean;
	};

	export type WorkflowOutput = {
	  status: string;
	};

	export default defineWorkflow<WorkflowInput, WorkflowOutput>({
	  callableName: "reviewCode",
	  async run(_ctx, input) {
	    return { status: input.reviewId };
	  },
		  async complete(_ctx, request) {
		    return request.mode === "field"
		      ? [{ type: "field", field: "reviewId" }]
		      : [{ type: "value", value: "review-1" }];
		  },
		  format(result, _request) {
		    return { markdown: result.status };
		  },
		});
	"#,
        ) {
            return;
        }

        let contract = extract_workflow_source_contract_from_typescript(workflow_dir.path())
            .expect("workflow source contract");

        assert_eq!(contract.callable_name.as_deref(), Some("reviewCode"));
        assert!(contract.format_hook);
        assert_eq!(
            contract.input_schema,
            json!({
                "type": "object",
                "properties": {
                    "reviewId": { "type": "string", "description": "Review identifier" },
                    "allowedAreas": {
                        "type": ["array", "null"],
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Review areas to include"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["json", "markdown"],
                        "description": "Output format"
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "minimum": 0,
                        "maximum": 10,
                        "description": "Maximum number of results"
                    },
                    "includeSkipped": {
                        "type": ["boolean", "null"],
                        "enum": [false, true],
                        "description": "Include skipped findings"
                    }
                },
                "required": ["reviewId", "format"],
                "additionalProperties": false
            })
        );
        assert!(contract.hooks.complete);
        assert_eq!(
            contract.format_schemas,
            BTreeMap::from([(
                "tui.markdown.v1".to_string(),
                json!({
                    "type": "object",
                    "properties": {
                        "markdown": { "type": "string" }
                    },
                    "required": ["markdown"],
                    "additionalProperties": false
                })
            )])
        );
    }

    #[test]
    fn extract_workflow_source_contract_from_typescript_rejects_anonymous_default_export() {
        let workflow_dir = TempDir::new().expect("workflow dir");
        if !write_workflow_source(
            workflow_dir.path(),
            r#"
export interface WorkflowInput {
  workflowId: string;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.status };
  },
};

export default async function (_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.workflowId };
}
"#,
        ) {
            return;
        }

        let err = extract_workflow_source_contract_from_typescript(workflow_dir.path())
            .expect_err("anonymous default export should be rejected");
        assert!(
            err.to_string()
                .contains("workflow default export must be a named function")
        );
    }

    #[test]
    fn extract_workflow_source_contract_from_typescript_rejects_recursive_types() {
        let workflow_dir = TempDir::new().expect("workflow dir");
        if !write_workflow_source(
            workflow_dir.path(),
            r#"
export interface WorkflowInput {
  workflowId: string;
  next?: WorkflowInput;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.status };
  },
};

export default async function codeReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.workflowId };
}
"#,
        ) {
            return;
        }

        let err = extract_workflow_source_contract_from_typescript(workflow_dir.path())
            .expect_err("recursive types should be rejected");
        assert!(
            err.to_string()
                .contains("recursive workflow API types are not supported")
        );
    }

    #[test]
    fn extract_workflow_source_contract_from_typescript_rejects_invalid_formatter_shape() {
        let workflow_dir = TempDir::new().expect("workflow dir");
        if !write_workflow_source(
            workflow_dir.path(),
            r#"
export interface WorkflowInput {
  workflowId: string;
}

export type WorkflowOutput = {
  status: string;
};

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { value: result.status };
  },
};

export default async function codeReview(_ctx: unknown, input: WorkflowInput): Promise<WorkflowOutput> {
  return { status: input.workflowId };
}
"#,
        ) {
            return;
        }

        let err = extract_workflow_source_contract_from_typescript(workflow_dir.path())
            .expect_err("invalid formatter return shape should be rejected");
        assert!(
            err.to_string()
                .contains("WorkflowOutput.toTuiMarkdown(result) must return { markdown: string }")
        );
    }
}
