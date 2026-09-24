const path = await import("node:path");
const fs = await import("node:fs");
const { builtinModules } = await import("node:module");
const { createInterface } = await import("node:readline");
const { pathToFileURL } = await import("node:url");

const CONTROL_PREFIX = "\u001eCODEX_WORKFLOW_CONTROL ";
const CONTROL_VERSION = 1;
const DRAFT_2020_12 = "https://json-schema.org/draft/2020-12/schema";
const INPUT_REQUEST_MAX_BYTES = 16 * 1024;
const INPUT_REQUEST_MAX_COUNT = 64;
const OUTPUT_MAX_BYTES = 8 * 1024;
const OUTPUT_JSON_MAX_BYTES = 1024 * 1024;
const RUN_CONTROL_FRAME_MAX_BYTES = 1024 * 1024;
const OUTPUT_TRUNCATION_NOTICE = "\n\n[Workflow output truncated to 8192 bytes.]";
const COMPLETION_REQUEST_MAX_BYTES = 64 * 1024;
const COMPLETION_OUTPUT_MAX_BYTES = 64 * 1024;
const COMPLETION_ERROR_MAX_BYTES = 4 * 1024;
const COMPLETION_ITEM_MAX_COUNT = 100;
const COMPLETION_VALUE_MAX_CHARS = 1_024;
const COMPLETION_DESCRIPTION_MAX_CHARS = 4_096;
const DYNAMIC_COMPLETION_TIMEOUT_MS = 1_500;
const INSPECTION_OUTPUT_MAX_BYTES = 256 * 1024;
const COMPLETION_OPERATION_OUTPUT_MAX_BYTES = INSPECTION_OUTPUT_MAX_BYTES + COMPLETION_OUTPUT_MAX_BYTES + COMPLETION_ERROR_MAX_BYTES + 1024;
const SOURCE_SCAN_MAX_BYTES = 1024 * 1024;
const SOURCE_SCAN_MAX_FILES = 256;
const SOURCE_SCAN_MAX_ENTRIES = 1024;
const SOURCE_SCAN_MAX_DEPTH = 32;
const SCHEMA_TRAVERSAL_MAX_DEPTH = 32;
const SCHEMA_TRAVERSAL_MAX_NODES = 2048;
const CONTROL_PATH = process.env.CODEX_WORKFLOW_CONTROL_PATH;
const CLI_MODE = process.env.CODEX_WORKFLOW_CLI === "1";
const BUILTIN_MODULES = new Set(builtinModules);

function byteLength(value) {
  return Buffer.byteLength(typeof value === "string" ? value : JSON.stringify(value));
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isPlainJsonObject(value) {
  if (!isObject(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function jsonEqual(left, right) {
  if (Object.is(left, right)) return true;
  if (typeof left !== typeof right || left === null || right === null) return false;
  if (Array.isArray(left)) {
    return Array.isArray(right)
      && left.length === right.length
      && left.every((value, index) => jsonEqual(value, right[index]));
  }
  if (!isObject(left) || !isObject(right)) return false;
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  return leftKeys.length === rightKeys.length
    && leftKeys.every((key, index) => key === rightKeys[index] && jsonEqual(left[key], right[key]));
}

function requireNonEmptyString(value, label) {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${label} must be a non-empty string.`);
  }
}

function assertJsonSerializable(value, label) {
  function assertJsonValue(candidate, path, seen) {
    if (candidate === null || typeof candidate === "string" || typeof candidate === "boolean") return;
    if (typeof candidate === "number" && Number.isFinite(candidate)) return;
    if (typeof candidate !== "object") {
      throw new Error(`${path} contains a non-JSON value.`);
    }
    if (seen.has(candidate)) throw new Error(`${path} contains a cycle.`);
    seen.add(candidate);
    if (Array.isArray(candidate)) {
      const ownKeys = Reflect.ownKeys(candidate);
      if (ownKeys.length !== candidate.length + 1
          || ownKeys.some((key) => key !== "length" && (typeof key !== "string" || !/^\d+$/.test(key)))) {
        throw new Error(`${path} must be a dense JSON array without extra properties.`);
      }
      for (let index = 0; index < candidate.length; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
        if (!descriptor || !("value" in descriptor)) {
          throw new Error(`${path} must be a dense JSON array without extra properties.`);
        }
        assertJsonValue(descriptor.value, `${path}[${index}]`, seen);
      }
    } else {
      if (!isPlainJsonObject(candidate)) throw new Error(`${path} must be a plain JSON object.`);
      for (const key of Reflect.ownKeys(candidate)) {
        const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
        if (typeof key !== "string" || !descriptor?.enumerable || !("value" in descriptor)) {
          throw new Error(`${path} contains a non-JSON object property.`);
        }
        assertJsonValue(descriptor.value, `${path}.${key}`, seen);
      }
    }
    seen.delete(candidate);
  }
  try {
    assertJsonValue(value, label, new Set());
    const encoded = JSON.stringify(value);
    if (encoded === undefined) throw new Error("value serializes to undefined");
    const decoded = JSON.parse(encoded);
    if (!jsonEqual(value, decoded)) throw new Error("value does not round-trip through JSON");
    return decoded;
  } catch (error) {
    throw new Error(`${label} must be JSON-serializable: ${error}`);
  }
}

function enterSchemaNode(budget, depth) {
  if (depth >= SCHEMA_TRAVERSAL_MAX_DEPTH || budget.remainingNodes === 0) {
    throw new Error("Workflow inputSchema exceeded the supported schema traversal limits.");
  }
  budget.remainingNodes -= 1;
}

function findSchemaAnchor(schema, anchor, budget, depth) {
  enterSchemaNode(budget, depth);
  if (isObject(schema)
      && (schema.$anchor === anchor || schema.$dynamicAnchor === anchor)) {
    return schema;
  }
  if (!isObject(schema) && !Array.isArray(schema)) return undefined;
  for (const child of Object.values(schema)) {
    if (child === null || typeof child !== "object") continue;
    const found = findSchemaAnchor(child, anchor, budget, depth + 1);
    if (found !== undefined) return found;
  }
  return undefined;
}

function resolveLocalSchemaRef(root, reference, budget, depth) {
  if (reference === "#") return root;
  if (!reference.startsWith("#")) return undefined;
  const fragment = reference.slice(1);
  if (!fragment.startsWith("/")) {
    return findSchemaAnchor(root, fragment, budget, depth);
  }
  let resolved = root;
  for (const encodedToken of fragment.slice(1).split("/")) {
    const token = encodedToken.replaceAll("~1", "/").replaceAll("~0", "~");
    if (resolved === null
        || typeof resolved !== "object"
        || !Object.prototype.hasOwnProperty.call(resolved, token)) {
      return undefined;
    }
    resolved = resolved[token];
  }
  return resolved;
}

function topLevelInputPropertyNames(root) {
  const properties = new Set();
  const activeRefs = new Set();
  const budget = { remainingNodes: SCHEMA_TRAVERSAL_MAX_NODES };

  function visit(schema, depth) {
    enterSchemaNode(budget, depth);
    if (!isObject(schema)) return;
    for (const property of Object.keys(schema.properties ?? {})) properties.add(property);
    for (const keyword of ["$ref", "$dynamicRef"]) {
      const reference = schema[keyword];
      const activeReference = `${keyword}:${reference}`;
      if (typeof reference !== "string" || activeRefs.has(activeReference)) continue;
      activeRefs.add(activeReference);
      const referenced = resolveLocalSchemaRef(root, reference, budget, depth + 1);
      if (referenced !== undefined) visit(referenced, depth + 1);
      activeRefs.delete(activeReference);
    }
    for (const keyword of ["allOf", "anyOf", "oneOf"]) {
      if (!Array.isArray(schema[keyword])) continue;
      for (const branch of schema[keyword]) visit(branch, depth + 1);
    }
  }

  visit(root, 0);
  return [...properties].sort();
}

async function loadWorkflow() {
  const moduleUrl = pathToFileURL(path.join(process.cwd(), "src", "workflow.ts")).href;
  const workflowModule = await import(moduleUrl);
  const workflow = workflowModule.default;
  if (!isObject(workflow)) throw new Error("Workflow must have a default object export.");
  if (workflow.apiVersion !== 1) throw new Error("Workflow default export must declare apiVersion: 1.");
  requireNonEmptyString(workflow.id, "Workflow id");
  requireNonEmptyString(workflow.title, "Workflow title");
  requireNonEmptyString(workflow.callableName, "Workflow callableName");
  if (!/^[a-z0-9_-]+(?:\/[a-z0-9_-]+)*$/.test(workflow.id)) {
    throw new Error("Workflow id must contain lowercase path components.");
  }
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(workflow.callableName)) {
    throw new Error("Workflow callableName must be lowercase and slash-command safe.");
  }
  if (typeof workflow.run !== "function") throw new Error("Workflow default export must define run(ctx, input).");
  if (workflow.complete !== undefined && typeof workflow.complete !== "function") throw new Error("Workflow complete must be a function when provided.");
  if (typeof workflow.format !== "function") throw new Error("Workflow default export must define format(output, options).");

  if (!Object.prototype.hasOwnProperty.call(workflowModule, "inputSchema")) {
    throw new Error("Workflow module must export named inputSchema.");
  }
  if (!Object.prototype.hasOwnProperty.call(workflowModule, "outputSchema")) {
    throw new Error("Workflow module must export named outputSchema.");
  }
  if (!Object.prototype.hasOwnProperty.call(workflow, "inputSchema")) {
    throw new Error("Workflow default export must define inputSchema.");
  }
  if (!Object.prototype.hasOwnProperty.call(workflow, "outputSchema")) {
    throw new Error("Workflow default export must define outputSchema.");
  }
  const inputSchema = assertJsonSerializable(workflow.inputSchema, "Workflow inputSchema");
  const outputSchema = assertJsonSerializable(workflow.outputSchema, "Workflow outputSchema");
  const namedInputSchema = assertJsonSerializable(workflowModule.inputSchema, "Named inputSchema export");
  const namedOutputSchema = assertJsonSerializable(workflowModule.outputSchema, "Named outputSchema export");
  if (!jsonEqual(inputSchema, namedInputSchema)) {
    throw new Error("Workflow default inputSchema must match the named inputSchema export.");
  }
  if (!jsonEqual(outputSchema, namedOutputSchema)) {
    throw new Error("Workflow default outputSchema must match the named outputSchema export.");
  }
  if (!isPlainJsonObject(inputSchema)
      || inputSchema.$schema !== DRAFT_2020_12) {
    throw new Error("Workflow inputSchema must be a Draft 2020-12 schema object.");
  }
  if (!isPlainJsonObject(outputSchema)
      || outputSchema.$schema !== DRAFT_2020_12) {
    throw new Error("Workflow outputSchema must be a Draft 2020-12 schema object.");
  }
  const inputTypes = Array.isArray(inputSchema.type)
    ? inputSchema.type
    : [inputSchema.type];
  if (!inputTypes.includes("object")) {
    throw new Error("Workflow inputSchema must describe a JSON object.");
  }
  const inputProperties = topLevelInputPropertyNames(inputSchema);
  if (inputSchema.additionalProperties === false
      && !inputProperties.includes("workingDirectory")) {
    throw new Error("Workflow inputSchema must accept the injected workingDirectory field.");
  }
  for (const property of inputProperties) {
    if (!/^[a-z][A-Za-z0-9]*$/.test(property)) {
      throw new Error(`Workflow inputSchema property ${JSON.stringify(property)} must be lower camelCase.`);
    }
  }
  return { workflow, inputSchema, outputSchema };
}

function emitEvent(event) {
  if (!CONTROL_PATH) throw new Error("Workflow control path is unavailable.");
  fs.appendFileSync(CONTROL_PATH, `${CONTROL_PREFIX}${JSON.stringify(event)}\n`);
}

let responseReader;
let pendingInputRequest;
let inputClosed = false;
let nextInputRequestId = 1;
let inputRequestCount = 0;
let inputRequestQueue = Promise.resolve();

function ensureResponseReader() {
  if (responseReader) return;
  responseReader = createInterface({ input: process.stdin, crlfDelay: Infinity });
  responseReader.on("line", (line) => {
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      rejectPendingInputRequest(`Workflow input channel returned invalid JSON: ${error}`);
      return;
    }
    if (!pendingInputRequest || message?.v !== CONTROL_VERSION || message.id !== pendingInputRequest.id) {
      rejectPendingInputRequest("Workflow input channel returned an invalid response frame.");
      return;
    }
    const pending = pendingInputRequest;
    pendingInputRequest = undefined;
    if (typeof message.error === "string") {
      pending.reject(new Error(message.error));
    } else if (Object.prototype.hasOwnProperty.call(message, "result")) {
      pending.resolve(message.result);
    } else {
      pending.reject(new Error("Workflow input response contained neither result nor error."));
    }
  });
  responseReader.on("close", () => {
    inputClosed = true;
    rejectPendingInputRequest("Workflow input channel closed before a response was received.");
  });
}

function rejectPendingInputRequest(message) {
  if (!pendingInputRequest) return;
  pendingInputRequest.reject(new Error(message));
  pendingInputRequest = undefined;
}

function issueControlRequest(event) {
  ensureResponseReader();
  if (inputClosed) return Promise.reject(new Error("Workflow input channel is closed."));
  return new Promise((resolve, reject) => {
    pendingInputRequest = { id: event.id, resolve, reject };
    emitEvent(event);
  });
}

function requestUserInput(params) {
  if (CLI_MODE || !CONTROL_PATH) {
    return Promise.reject(new Error("requestUserInput is only available during hosted workflow execution."));
  }
  let snapshot;
  try {
    const serialized = JSON.stringify(params);
    snapshot = JSON.parse(serialized);
  } catch (error) {
    return Promise.reject(new Error(`Workflow input request must be JSON-serializable: ${error}`));
  }
  const id = nextInputRequestId;
  if (inputRequestCount >= INPUT_REQUEST_MAX_COUNT) {
    return Promise.reject(new Error(`Workflow exceeded ${INPUT_REQUEST_MAX_COUNT} user input requests.`));
  }
  const event = { v: CONTROL_VERSION, id, method: "requestUserInput", params: snapshot };
  if (byteLength(event) > INPUT_REQUEST_MAX_BYTES) {
    return Promise.reject(new Error(`Workflow input request exceeded ${INPUT_REQUEST_MAX_BYTES} bytes.`));
  }
  nextInputRequestId += 1;
  inputRequestCount += 1;
  const request = inputRequestQueue.then(() => issueControlRequest(event));
  inputRequestQueue = request.then(() => undefined, () => undefined);
  return request;
}

function requestOutputValidation(output) {
  if (!CONTROL_PATH) {
    return Promise.reject(new Error("Workflow output validation channel is unavailable."));
  }
  const event = {
    v: CONTROL_VERSION,
    id: nextInputRequestId,
    method: "validateOutput",
    params: { output },
  };
  if (byteLength(event) > OUTPUT_JSON_MAX_BYTES) {
    return Promise.reject(new Error(`Workflow output exceeded ${OUTPUT_JSON_MAX_BYTES} bytes.`));
  }
  nextInputRequestId += 1;
  return issueControlRequest(event);
}

function requestContractValidation(inputSchema, outputSchema) {
  if (!CONTROL_PATH) {
    return Promise.reject(new Error("Workflow contract validation channel is unavailable."));
  }
  const event = {
    v: CONTROL_VERSION,
    id: nextInputRequestId,
    method: "contract",
    params: {
      inputSchema,
      outputSchema,
    },
  };
  if (byteLength(event) > RUN_CONTROL_FRAME_MAX_BYTES) {
    return Promise.reject(new Error(`Workflow contract exceeded ${RUN_CONTROL_FRAME_MAX_BYTES} bytes.`));
  }
  nextInputRequestId += 1;
  return issueControlRequest(event);
}

function progress(message, data) {
  requireNonEmptyString(message, "Workflow progress message");
  if (data !== undefined) assertJsonSerializable(data, "Workflow progress data");
  // Hosted workflow progress does not yet have a stable app-server event. Keep
  // it off the request/response control channel, where it could be mistaken
  // for an interactive request. Standalone CLI runs surface it on stderr.
  if (CLI_MODE || !CONTROL_PATH) {
    const suffix = data === undefined ? "" : ` ${JSON.stringify(data)}`;
    console.error(`${message}${suffix}`);
  }
}

function createContext(input, allowInteraction) {
  const context = { progress };
  if (allowInteraction) context.requestUserInput = requestUserInput;
  if (isPlainJsonObject(input) && typeof input.workingDirectory === "string") {
    context.workingDirectory = input.workingDirectory;
    context.cwd = input.workingDirectory;
    context.currentWorkingDirectory = input.workingDirectory;
    context.repoRoot = input.workingDirectory;
  }
  return context;
}

function truncateWorkflowOutput(markdown) {
  if (!markdown.endsWith("\n")) markdown += "\n";
  const bytes = Buffer.from(markdown);
  const decoder = new TextDecoder("utf-8", { fatal: true });
  if (bytes.length <= OUTPUT_MAX_BYTES) return decoder.decode(bytes);
  const maxPrefixBytes = OUTPUT_MAX_BYTES - byteLength(OUTPUT_TRUNCATION_NOTICE);
  for (let end = maxPrefixBytes; end >= 0; end -= 1) {
    try {
      return decoder.decode(bytes.subarray(0, end)) + OUTPUT_TRUNCATION_NOTICE;
    } catch {}
  }
  return OUTPUT_TRUNCATION_NOTICE;
}

function truncateUtf8(value, maximumBytes) {
  const bytes = Buffer.from(value);
  if (bytes.length <= maximumBytes) return value;
  const decoder = new TextDecoder("utf-8", { fatal: true });
  for (let end = maximumBytes; end >= 0; end -= 1) {
    try {
      return decoder.decode(bytes.subarray(0, end));
    } catch {}
  }
  return "";
}

function parseJsonArgument(raw, label) {
  let value;
  try {
    value = JSON.parse(raw ?? "");
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${error}`);
  }
  return value;
}

function workflowInspection(workflow, inputSchema, outputSchema) {
  return {
    apiVersion: workflow.apiVersion,
    id: workflow.id,
    title: workflow.title,
    callableName: workflow.callableName,
    inputSchema,
    outputSchema,
    hasComplete: typeof workflow.complete === "function",
  };
}

function scanWorkflowSources() {
  const root = path.join(process.cwd(), "src");
  const sources = [];
  let totalBytes = 0;
  let totalEntries = 0;
  function visit(directory, depth) {
    if (depth > SOURCE_SCAN_MAX_DEPTH) {
      throw new Error(`Workflow source exceeds ${SOURCE_SCAN_MAX_DEPTH} directory levels.`);
    }
    const handle = fs.opendirSync(directory);
    const entries = [];
    try {
      let entry;
      while ((entry = handle.readSync()) !== null) {
        totalEntries += 1;
        if (totalEntries > SOURCE_SCAN_MAX_ENTRIES) {
          throw new Error(`Workflow source exceeds ${SOURCE_SCAN_MAX_ENTRIES} directory entries.`);
        }
        entries.push(entry);
      }
    } finally {
      handle.closeSync();
    }
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue;
      const entryPath = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        visit(entryPath, depth + 1);
        continue;
      }
      if (!entry.isFile() || !/\.(?:[cm]?[jt]sx?)$/.test(entry.name)) continue;
      if (sources.length >= SOURCE_SCAN_MAX_FILES) {
        throw new Error(`Workflow source exceeds ${SOURCE_SCAN_MAX_FILES} TypeScript files.`);
      }
      const remaining = SOURCE_SCAN_MAX_BYTES - totalBytes;
      const metadata = fs.lstatSync(entryPath);
      if (!metadata.isFile() || metadata.size > remaining) {
        throw new Error(`Workflow source exceeds ${SOURCE_SCAN_MAX_BYTES} bytes.`);
      }
      const source = fs.readFileSync(entryPath, "utf8");
      totalBytes += byteLength(source);
      if (totalBytes > SOURCE_SCAN_MAX_BYTES) {
        throw new Error(`Workflow source exceeds ${SOURCE_SCAN_MAX_BYTES} bytes.`);
      }
      const extension = path.extname(entry.name);
      const loader = extension.endsWith("x")
        ? (extension.includes("t") ? "tsx" : "jsx")
        : (extension.includes("t") ? "ts" : "js");
      const scan = new Bun.Transpiler({ loader }).scan(source);
      sources.push({
        path: path.relative(process.cwd(), entryPath).split(path.sep).join("/"),
        exports: scan.exports,
        imports: scan.imports.map((sourceImport) => ({
          ...sourceImport,
          builtin: BUILTIN_MODULES.has(sourceImport.path),
        })),
      });
    }
  }
  visit(root, 0);
  return sources;
}

function validateExpectedManifest(workflow, rawExpected) {
  if (rawExpected === undefined) return;
  const expected = parseJsonArgument(rawExpected, "Expected workflow manifest");
  if (!isPlainJsonObject(expected)) throw new Error("Expected workflow manifest must be a JSON object.");
  for (const [field, label] of [
    ["apiVersion", "apiVersion"],
    ["id", "id"],
    ["title", "title"],
    ["callableName", "callableName"],
  ]) {
    if (!Object.is(workflow[field], expected[field])) {
      throw new Error(`Workflow module ${label} ${JSON.stringify(workflow[field])} does not match workflow.yaml ${label} ${JSON.stringify(expected[field])}.`);
    }
  }
}

function camelToKebab(value) {
  return value
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replaceAll("_", "-")
    .toLowerCase();
}

function schemaDescription(schema) {
  return isObject(schema) && typeof schema.description === "string" ? schema.description : undefined;
}

function completionScalar(value) {
  return typeof value === "string" ? value : JSON.stringify(value);
}

function staticCompletions(workflow, request) {
  const properties = workflow.inputSchema.properties ?? {};
  if (request.mode === "field") {
    return Object.entries(properties)
      .map(([name, schema]) => ({
        value: `--${camelToKebab(name)}`,
        description: schemaDescription(schema),
      }))
      .filter((item) => item.value.startsWith(request.prefix));
  }
  if (request.activeField === undefined) return [];
  const schema = properties[request.activeField];
  if (!isObject(schema)) return [];
  const values = schema.const !== undefined ? [schema.const] : (schema.enum ?? []);
  return values
    .map((value) => ({ value: completionScalar(value), description: schemaDescription(schema) }))
    .filter((item) => item.value.startsWith(request.prefix));
}

function validateCompletionRequest(request) {
  if (!isPlainJsonObject(request)) throw new Error("Completion request must be a JSON object.");
  if (!isPlainJsonObject(request.input)) throw new Error("Completion request input must be a JSON object.");
  if (request.activeField === null) delete request.activeField;
  if (request.activeField !== undefined && typeof request.activeField !== "string") throw new Error("Completion request activeField must be a string when provided.");
  if (typeof request.prefix !== "string") throw new Error("Completion request prefix must be a string.");
  if (request.mode !== "field" && request.mode !== "value") throw new Error('Completion request mode must be "field" or "value".');
}

function normalizeCompletionItems(items, source) {
  if (!Array.isArray(items)) throw new Error(`${source} completions must be an array.`);
  return items.map((item, index) => {
    if (!isPlainJsonObject(item)) throw new Error(`${source} completion ${index} must be an object.`);
    requireNonEmptyString(item.value, `${source} completion ${index} value`);
    if ([...item.value].length > COMPLETION_VALUE_MAX_CHARS) throw new Error(`${source} completion ${index} value is too long.`);
    if (item.description !== undefined && typeof item.description !== "string") throw new Error(`${source} completion ${index} description must be a string.`);
    if (item.description !== undefined && [...item.description].length > COMPLETION_DESCRIPTION_MAX_CHARS) throw new Error(`${source} completion ${index} description is too long.`);
    return item.description === undefined
      ? { value: item.value }
      : { value: item.value, description: item.description };
  });
}

function withTimeout(promise, timeoutMs, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs} ms.`)), timeoutMs);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function boundCompletionItems(items) {
  const output = [];
  const values = new Set();
  for (const item of items) {
    if (output.length >= COMPLETION_ITEM_MAX_COUNT || values.has(item.value)) continue;
    const candidate = [...output, item];
    if (byteLength(candidate) > COMPLETION_OUTPUT_MAX_BYTES) break;
    values.add(item.value);
    output.push(item);
  }
  return output;
}

async function runCompletion(workflow, request) {
  validateCompletionRequest(request);
  if (typeof workflow.complete !== "function") return [];
  const result = await withTimeout(
    Promise.resolve(workflow.complete(createContext(request.input, false), request)),
    DYNAMIC_COMPLETION_TIMEOUT_MS,
    "Workflow completion",
  );
  return boundCompletionItems(normalizeCompletionItems(result, "Dynamic"));
}

function writeBoundedJson(value, maximumBytes, label) {
  const encoded = JSON.stringify(value);
  if (byteLength(encoded) > maximumBytes) throw new Error(`${label} exceeded ${maximumBytes} bytes.`);
  process.stdout.write(`${encoded}\n`);
}

async function executeRun(workflow, inputSchema, outputSchema, rawInput) {
  const input = parseJsonArgument(rawInput ?? "{}", "Workflow input");
  if (!isPlainJsonObject(input)) throw new Error("Workflow input must be a JSON object.");
  await requestContractValidation(inputSchema, outputSchema);
  const output = await workflow.run(createContext(input, !CLI_MODE && Boolean(CONTROL_PATH)), input);
  await inputRequestQueue;
  const normalizedOutput = assertJsonSerializable(output, "Workflow output");
  await requestOutputValidation(normalizedOutput);
  const formatted = await workflow.format(normalizedOutput, { format: "markdown.v1" });
  if (!isPlainJsonObject(formatted) || typeof formatted.markdown !== "string") {
    throw new Error("Workflow formatter must return { markdown: string } for markdown.v1.");
  }
  const markdown = truncateWorkflowOutput(formatted.markdown);
  if (!CONTROL_PATH) {
    process.stdout.write(markdown);
    return;
  }
  await issueControlRequest({
    v: CONTROL_VERSION,
    id: 0,
    method: "complete",
    params: { markdown },
  });
}

function readRunnerFile(filePath, label) {
  if (filePath === undefined || filePath === "-") return undefined;
  try {
    return fs.readFileSync(filePath, "utf8");
  } catch (error) {
    throw new Error(`Failed to read ${label}: ${error}`);
  }
}

try {
  const operation = process.argv[2];
  const payload = readRunnerFile(process.argv[3], "workflow runner payload");
  const expectedManifestJson = readRunnerFile(process.argv[4], "expected workflow manifest");
  if (operation === "scan") {
    writeBoundedJson(scanWorkflowSources(), INSPECTION_OUTPUT_MAX_BYTES, "Workflow source scan output");
  } else {
    const { workflow, inputSchema, outputSchema } = await loadWorkflow();
    validateExpectedManifest(workflow, expectedManifestJson);
    switch (operation) {
    case "run":
      await executeRun(workflow, inputSchema, outputSchema, payload);
      break;
    case "inspect":
      writeBoundedJson(workflowInspection(workflow, inputSchema, outputSchema), INSPECTION_OUTPUT_MAX_BYTES, "Workflow inspection output");
      break;
    case "complete": {
      if (byteLength(payload ?? "") > COMPLETION_REQUEST_MAX_BYTES) {
        throw new Error(`Workflow completion request exceeded ${COMPLETION_REQUEST_MAX_BYTES} bytes.`);
      }
      const request = parseJsonArgument(payload, "Workflow completion request");
      let items = [];
      let error;
      try {
        items = await runCompletion(workflow, request);
      } catch (completionError) {
        error = truncateUtf8(
          completionError instanceof Error ? completionError.message : String(completionError),
          COMPLETION_ERROR_MAX_BYTES,
        );
      }
      const inspection = workflowInspection(workflow, inputSchema, outputSchema);
      if (byteLength(inspection) > INSPECTION_OUTPUT_MAX_BYTES) {
        throw new Error(`Workflow inspection output exceeded ${INSPECTION_OUTPUT_MAX_BYTES} bytes.`);
      }
      writeBoundedJson(
        { inspection, items, error },
        COMPLETION_OPERATION_OUTPUT_MAX_BYTES,
        "Workflow completion output",
      );
      break;
    }
    default:
      throw new Error(`Unsupported workflow runner operation ${JSON.stringify(operation)}.`);
    }
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
} finally {
  responseReader?.close();
}
