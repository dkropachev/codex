import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";

export type OutputSchemaFile = {
  schemaPath?: string;
  cleanup: () => Promise<void>;
};

export async function createOutputSchemaFile(schema: unknown): Promise<OutputSchemaFile> {
  if (schema === undefined) {
    return { cleanup: async () => {} };
  }

  validateOutputSchema(schema);

  const schemaDir = await fs.mkdtemp(path.join(os.tmpdir(), "codex-output-schema-"));
  const schemaPath = path.join(schemaDir, "schema.json");
  const cleanup = async () => {
    try {
      await fs.rm(schemaDir, { recursive: true, force: true });
    } catch {
      // suppress
    }
  };

  try {
    await fs.writeFile(schemaPath, JSON.stringify(schema), "utf8");
    return { schemaPath, cleanup };
  } catch (error) {
    await cleanup();
    throw error;
  }
}

function validateOutputSchema(schema: unknown): void {
  if (!isJsonObject(schema)) {
    throw new Error("outputSchema must be a plain JSON object");
  }

  validateSchemaNode(schema, "$", new WeakSet<object>());
}

function validateSchemaNode(value: unknown, path: string, stack: WeakSet<object>): void {
  if (Array.isArray(value)) {
    if (stack.has(value)) {
      throw new Error(`outputSchema at ${path} contains a cycle`);
    }

    stack.add(value);
    try {
      for (let index = 0; index < value.length; index += 1) {
        validateSchemaNode(value[index], `${path}[${index}]`, stack);
      }
    } finally {
      stack.delete(value);
    }

    return;
  }

  if (!isJsonObject(value)) {
    return;
  }

  if (stack.has(value)) {
    throw new Error(`outputSchema at ${path} contains a cycle`);
  }

  stack.add(value);
  try {
    if (Object.prototype.hasOwnProperty.call(value, "properties")) {
      validateObjectSchema(value, path);
    }

    for (const [key, child] of Object.entries(value)) {
      validateSchemaNode(child, `${path}.${key}`, stack);
    }
  } finally {
    stack.delete(value);
  }
}

function validateObjectSchema(schema: Record<string, unknown>, path: string): void {
  const properties = schema.properties;
  if (!isJsonObject(properties)) {
    throw new Error(`outputSchema at ${path} must define properties as a plain JSON object`);
  }

  const required = schema.required;
  if (!Array.isArray(required) || required.some((entry) => typeof entry !== "string")) {
    throw new Error(`outputSchema at ${path} must define required as an array of strings`);
  }

  const propertyNames = Object.keys(properties);
  const requiredNames = new Set(required);
  const missingRequired = propertyNames.filter((name) => !requiredNames.has(name));
  const extraRequired = required.filter((name) => !Object.prototype.hasOwnProperty.call(properties, name));
  if (missingRequired.length > 0 || extraRequired.length > 0) {
    const details: string[] = [];
    if (missingRequired.length > 0) {
      details.push(`missing required keys: ${missingRequired.join(", ")}`);
    }
    if (extraRequired.length > 0) {
      details.push(`required references unknown keys: ${extraRequired.join(", ")}`);
    }
    throw new Error(`outputSchema at ${path} must list every property in required (${details.join("; ")})`);
  }

  if (schema.additionalProperties !== false) {
    throw new Error(`outputSchema at ${path} must set additionalProperties to false`);
  }
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
