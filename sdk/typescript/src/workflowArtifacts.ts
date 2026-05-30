import { createHash } from "node:crypto";
import { constants as fsConstants } from "node:fs";
import { access, lstat, mkdir, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

import type { AppServerClient } from "./appServerClient";

type ArtifactSource = {
  path: string;
  kind: string;
  sha256: string;
};

type ArtifactStateRegisterParams = {
  namespace: string;
  scopeKey: string;
  sourceKey: string;
  stateDir: string;
  sources: ArtifactSource[];
  metadata: unknown;
};

type ArtifactStateHitParams = {
  namespace: string;
  stateDir: string;
};

type ArtifactCacheStorageEntry = {
  namespace: string;
  key: string;
  artifactId: string;
  status: string;
  metadata: unknown;
  createdAtUnixSec: number;
  updatedAtUnixSec: number;
  lastHitAtUnixSec: number | null;
};

type ArtifactCacheReadParams = {
  namespace: string;
  key: string;
};

type ArtifactCacheReadResponse = {
  entry: ArtifactCacheStorageEntry | null;
};

type ArtifactCacheWriteParams = {
  namespace: string;
  key: string;
  artifactId: string;
  status: string;
  metadata: unknown;
};

type ArtifactCacheWriteResponse = {
  entry: ArtifactCacheStorageEntry;
};

type ArtifactCacheStoredMetadata = {
  schemaVersion: 1;
  outputDir: string;
  retention: ArtifactCacheRetention;
  userMetadata: unknown;
  scope: ArtifactCacheScopeSnapshot;
};

type ArtifactCacheNormalizedOptions = {
  namespace: string;
  key: string;
  scope: {
    root: string;
    include: string[];
    exclude: string[];
  };
  output: {
    dir: string | null;
    retention: ArtifactCacheRetention;
  };
  refresh: "always" | "auto";
  build: ArtifactCacheBuildFunction;
};

type ScopeDiff = {
  files: ArtifactScopeFileChange[];
  definitionChanged: boolean;
  hasStructuralChanges: boolean;
};

export type ArtifactCacheRetention = "ephemeral" | "persistent";

export type ArtifactCacheBuildReason =
  | "initial"
  | "scopeChanged"
  | "outputMissing"
  | "manualRefresh";

export type ArtifactCacheResultReason = ArtifactCacheBuildReason | "cacheHit";

export type ArtifactCacheScope = {
  root?: string;
  include: string[];
  exclude?: string[];
};

export type ArtifactCacheOutput = {
  dir?: string;
  retention?: ArtifactCacheRetention;
};

export type ArtifactScopeFile = {
  path: string;
  sha256: string;
  sizeBytes: number;
};

export type ArtifactScopeFileChange = {
  path: string;
  change: "added" | "modified" | "deleted";
  oldSha256?: string;
  newSha256?: string;
};

export type ArtifactCacheScopeSnapshot = {
  root: string;
  include: string[];
  exclude: string[];
  hash: string;
  files: ArtifactScopeFile[];
};

export type ArtifactCacheEntry = {
  namespace: string;
  key: string;
  artifactId: string;
  status: string;
  outputDir: string;
  retention: ArtifactCacheRetention;
  metadata: unknown;
  scope: ArtifactCacheScopeSnapshot;
  createdAtUnixSec: number;
  updatedAtUnixSec: number;
  lastHitAtUnixSec: number | null;
};

export type ArtifactBuildContext = {
  outputDir: string;
  reason: ArtifactCacheBuildReason;
  scope: {
    hash: string;
    previousHash: string | null;
    files: ArtifactScopeFile[];
    changed: ArtifactScopeFileChange[];
    definitionChanged: boolean;
    hasStructuralChanges: boolean;
  };
  previous: ArtifactCacheEntry | null;
};

export type ArtifactBuildResult = {
  status?: string;
  metadata?: unknown;
};

export type ArtifactCacheBuildFunction = (
  context: ArtifactBuildContext,
) => void | ArtifactBuildResult | Promise<void | ArtifactBuildResult>;

export type ArtifactCacheEnsureOptions = {
  namespace?: string;
  key: string;
  scope: ArtifactCacheScope;
  output?: ArtifactCacheOutput;
  refresh?: "always";
  build: ArtifactCacheBuildFunction;
};

export type ArtifactCacheArtifact = ArtifactCacheEntry & {
  rebuilt: boolean;
  reason: ArtifactCacheResultReason;
  path(relativePath?: string): string;
};

export class WorkflowArtifacts {
  readonly cache: WorkflowArtifactCache;

  constructor(client: AppServerClient) {
    this.cache = new WorkflowArtifactCache(client);
  }
}

export class WorkflowArtifactCache {
  constructor(private client: AppServerClient) {}

  async ensure(options: ArtifactCacheEnsureOptions): Promise<ArtifactCacheArtifact> {
    const normalized = normalizeOptions(options);
    const currentScope = await collectScopeSnapshot(normalized.scope);
    const previousStorage = await this.readCacheEntry({
      namespace: normalized.namespace,
      key: normalized.key,
    });
    const previous = artifactCacheEntryFromStorage(previousStorage.entry);
    const scope = { ...currentScope, hash: hashScope(currentScope) };
    const outputDir = resolveOutputDir(normalized, scope.hash, this.client.codexHome);
    const diff = diffScope(previous?.scope ?? null, scope);
    const reason = await buildReason(normalized, previous, scope, outputDir);

    if (!reason && previous) {
      await this.recordStateHit({ namespace: normalized.namespace, stateDir: previous.outputDir });
      return artifactFromEntry(previous, false, "cacheHit");
    }

    const buildReasonValue = reason ?? "initial";
    await prepareOutputDir(outputDir, normalized.output.dir === null);
    const buildResult = await normalized.build({
      outputDir,
      reason: buildReasonValue,
      scope: {
        hash: scope.hash,
        previousHash: previous?.scope.hash ?? null,
        files: scope.files,
        changed: diff.files,
        definitionChanged: diff.definitionChanged,
        hasStructuralChanges: diff.hasStructuralChanges,
      },
      previous,
    });

    const metadata: ArtifactCacheStoredMetadata = {
      schemaVersion: 1,
      outputDir,
      retention: normalized.output.retention,
      userMetadata: isRecord(buildResult) ? buildResult.metadata : undefined,
      scope,
    };
    const status =
      isRecord(buildResult) && typeof buildResult.status === "string"
        ? buildResult.status
        : "fresh";

    await this.registerState({
      namespace: normalized.namespace,
      scopeKey: normalized.key,
      sourceKey: scope.hash,
      stateDir: outputDir,
      sources: scope.files.map((file) => ({
        path: file.path,
        kind: "scope",
        sha256: file.sha256,
      })),
      metadata,
    });
    await this.recordStateHit({ namespace: normalized.namespace, stateDir: outputDir });
    const written = await this.writeCacheEntry({
      namespace: normalized.namespace,
      key: normalized.key,
      artifactId: scope.hash,
      status,
      metadata,
    });
    const entry = artifactCacheEntryFromStorage(written.entry);
    if (!entry) {
      throw new Error("artifact cache entry was not readable after writing");
    }
    return artifactFromEntry(entry, true, buildReasonValue);
  }

  private readCacheEntry(params: ArtifactCacheReadParams): Promise<ArtifactCacheReadResponse> {
    return this.client.request("artifact/cache/read", params);
  }

  private writeCacheEntry(params: ArtifactCacheWriteParams): Promise<ArtifactCacheWriteResponse> {
    return this.client.request("artifact/cache/write", params);
  }

  private registerState(params: ArtifactStateRegisterParams): Promise<unknown> {
    return this.client.request("artifact/state/register", params);
  }

  private recordStateHit(params: ArtifactStateHitParams): Promise<unknown> {
    return this.client.request("artifact/state/hit", params);
  }
}

function normalizeOptions(options: ArtifactCacheEnsureOptions): ArtifactCacheNormalizedOptions {
  const root = path.resolve(options.scope.root ?? process.cwd());
  const outputDir = options.output?.dir ? options.output.dir : null;
  return {
    namespace: options.namespace ?? "workflow",
    key: options.key,
    scope: {
      root,
      include: options.scope.include.map(normalizePattern),
      exclude: (options.scope.exclude ?? []).map(normalizePattern),
    },
    output: {
      dir: outputDir,
      retention: options.output?.retention ?? (outputDir ? "persistent" : "ephemeral"),
    },
    refresh: options.refresh ?? "auto",
    build: options.build,
  };
}

async function collectScopeSnapshot(
  scope: ArtifactCacheNormalizedOptions["scope"],
): Promise<Omit<ArtifactCacheScopeSnapshot, "hash">> {
  if (scope.include.length === 0) {
    throw new Error("artifact cache scope.include must contain at least one path or glob");
  }
  const files = new Map<string, ArtifactScopeFile>();
  const excludeRegexes = scope.exclude.map(globToRegExp);

  for (const include of scope.include) {
    await collectIncludePattern(scope.root, include, excludeRegexes, files);
  }

  return {
    root: scope.root,
    include: [...scope.include],
    exclude: [...scope.exclude],
    files: [...files.values()].sort((left, right) => left.path.localeCompare(right.path)),
  };
}

async function collectIncludePattern(
  root: string,
  include: string,
  excludeRegexes: RegExp[],
  files: Map<string, ArtifactScopeFile>,
): Promise<void> {
  if (!hasGlob(include)) {
    const target = path.resolve(root, include);
    ensureWithinRoot(root, target);
    if (!(await pathExists(target))) {
      return;
    }
    await collectPath(root, target, null, excludeRegexes, files);
    return;
  }

  const includeRegex = globToRegExp(include);
  const base = path.resolve(root, globStaticBase(include));
  ensureWithinRoot(root, base);
  if (!(await pathExists(base))) {
    return;
  }
  await collectPath(root, base, includeRegex, excludeRegexes, files);
}

async function collectPath(
  root: string,
  absolutePath: string,
  includeRegex: RegExp | null,
  excludeRegexes: RegExp[],
  files: Map<string, ArtifactScopeFile>,
): Promise<void> {
  const info = await lstat(absolutePath);
  const relativePath = relativeToRoot(root, absolutePath);
  if (relativePath && matchesAny(relativePath, excludeRegexes, info.isDirectory())) {
    return;
  }
  if (info.isDirectory()) {
    const entries = (await readdir(absolutePath, { withFileTypes: true })).sort((left, right) =>
      left.name.localeCompare(right.name),
    );
    for (const entry of entries) {
      await collectPath(
        root,
        path.join(absolutePath, entry.name),
        includeRegex,
        excludeRegexes,
        files,
      );
    }
    return;
  }
  if (!info.isFile()) {
    return;
  }
  if (includeRegex && !includeRegex.test(relativePath)) {
    return;
  }
  const bytes = await readFile(absolutePath);
  files.set(relativePath, {
    path: relativePath,
    sha256: createHash("sha256").update(bytes).digest("hex"),
    sizeBytes: bytes.byteLength,
  });
}

function hashScope(scope: Omit<ArtifactCacheScopeSnapshot, "hash">): string {
  const hash = createHash("sha256");
  hash.update(
    JSON.stringify({
      root: scope.root,
      include: scope.include,
      exclude: scope.exclude,
    }),
  );
  hash.update("\0");
  for (const file of scope.files) {
    hash.update(file.path);
    hash.update("\0");
    hash.update(file.sha256);
    hash.update("\0");
    hash.update(String(file.sizeBytes));
    hash.update("\0");
  }
  return hash.digest("hex");
}

function diffScope(
  previous: ArtifactCacheScopeSnapshot | null,
  current: ArtifactCacheScopeSnapshot,
): ScopeDiff {
  const previousFiles = new Map((previous?.files ?? []).map((file) => [file.path, file]));
  const currentFiles = new Map(current.files.map((file) => [file.path, file]));
  const changes: ArtifactScopeFileChange[] = [];

  for (const [filePath, file] of currentFiles) {
    const oldFile = previousFiles.get(filePath);
    if (!oldFile) {
      changes.push({ path: filePath, change: "added", newSha256: file.sha256 });
    } else if (oldFile.sha256 !== file.sha256) {
      changes.push({
        path: filePath,
        change: "modified",
        oldSha256: oldFile.sha256,
        newSha256: file.sha256,
      });
    }
  }

  for (const [filePath, file] of previousFiles) {
    if (!currentFiles.has(filePath)) {
      changes.push({ path: filePath, change: "deleted", oldSha256: file.sha256 });
    }
  }

  const definitionChanged =
    previous !== null &&
    (previous.root !== current.root ||
      !sameStringArray(previous.include, current.include) ||
      !sameStringArray(previous.exclude, current.exclude));
  const hasStructuralChanges =
    definitionChanged ||
    changes.some((change) => change.change === "added" || change.change === "deleted");

  return {
    files: changes.sort((left, right) => left.path.localeCompare(right.path)),
    definitionChanged,
    hasStructuralChanges,
  };
}

async function buildReason(
  options: ArtifactCacheNormalizedOptions,
  previous: ArtifactCacheEntry | null,
  scope: ArtifactCacheScopeSnapshot,
  outputDir: string,
): Promise<ArtifactCacheBuildReason | null> {
  if (options.refresh === "always") {
    return "manualRefresh";
  }
  if (!previous) {
    return "initial";
  }
  if (previous.scope.hash !== scope.hash) {
    return "scopeChanged";
  }
  if (previous.outputDir !== outputDir || previous.retention !== options.output.retention) {
    return "outputMissing";
  }
  if (!(await pathExists(outputDir))) {
    return "outputMissing";
  }
  return null;
}

async function prepareOutputDir(outputDir: string, managedOutput: boolean): Promise<void> {
  if (managedOutput) {
    await rm(outputDir, { recursive: true, force: true });
  }
  await mkdir(outputDir, { recursive: true });
}

function resolveOutputDir(
  options: ArtifactCacheNormalizedOptions,
  scopeHash: string,
  codexHome: string | null,
): string {
  if (options.output.dir) {
    return path.isAbsolute(options.output.dir)
      ? options.output.dir
      : path.resolve(options.scope.root, options.output.dir);
  }

  const root = codexHome
    ? path.join(codexHome, ".tmp", "workflow-artifacts")
    : path.join(tmpdir(), "codex-workflow-artifacts");
  return path.join(
    root,
    safePathSegment(options.namespace),
    safePathSegment(options.key),
    scopeHash,
  );
}

function artifactCacheEntryFromStorage(
  entry: ArtifactCacheStorageEntry | null,
): ArtifactCacheEntry | null {
  if (!entry) {
    return null;
  }
  const metadata = storedMetadataFromUnknown(entry.metadata);
  if (!metadata) {
    return null;
  }
  return {
    namespace: entry.namespace,
    key: entry.key,
    artifactId: entry.artifactId,
    status: entry.status,
    outputDir: metadata.outputDir,
    retention: metadata.retention,
    metadata: metadata.userMetadata,
    scope: metadata.scope,
    createdAtUnixSec: entry.createdAtUnixSec,
    updatedAtUnixSec: entry.updatedAtUnixSec,
    lastHitAtUnixSec: entry.lastHitAtUnixSec,
  };
}

function artifactFromEntry(
  entry: ArtifactCacheEntry,
  rebuilt: boolean,
  reason: ArtifactCacheResultReason,
): ArtifactCacheArtifact {
  return {
    ...entry,
    rebuilt,
    reason,
    path(relativePath = "") {
      return relativePath ? path.join(entry.outputDir, relativePath) : entry.outputDir;
    },
  };
}

function storedMetadataFromUnknown(value: unknown): ArtifactCacheStoredMetadata | null {
  if (!isRecord(value) || value.schemaVersion !== 1) {
    return null;
  }
  if (
    typeof value.outputDir !== "string" ||
    (value.retention !== "ephemeral" && value.retention !== "persistent") ||
    !isScopeSnapshot(value.scope)
  ) {
    return null;
  }
  return {
    schemaVersion: 1,
    outputDir: value.outputDir,
    retention: value.retention,
    userMetadata: value.userMetadata,
    scope: value.scope,
  };
}

function isScopeSnapshot(value: unknown): value is ArtifactCacheScopeSnapshot {
  return (
    isRecord(value) &&
    typeof value.root === "string" &&
    Array.isArray(value.include) &&
    value.include.every((item) => typeof item === "string") &&
    Array.isArray(value.exclude) &&
    value.exclude.every((item) => typeof item === "string") &&
    typeof value.hash === "string" &&
    Array.isArray(value.files) &&
    value.files.every(isScopeFile)
  );
}

function isScopeFile(value: unknown): value is ArtifactScopeFile {
  return (
    isRecord(value) &&
    typeof value.path === "string" &&
    typeof value.sha256 === "string" &&
    typeof value.sizeBytes === "number"
  );
}

function sameStringArray(left: string[], right: string[]): boolean {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function normalizePattern(pattern: string): string {
  return pattern.replace(/\\/g, "/").replace(/^\.\//, "").replace(/\/+$/, "");
}

function hasGlob(pattern: string): boolean {
  return pattern.includes("*") || pattern.includes("?");
}

function globStaticBase(pattern: string): string {
  const segments = pattern.split("/");
  const base: string[] = [];
  for (const segment of segments) {
    if (hasGlob(segment)) {
      break;
    }
    base.push(segment);
  }
  return base.length === 0 ? "." : base.join("/");
}

function globToRegExp(pattern: string): RegExp {
  const segments = normalizePattern(pattern).split("/").filter(Boolean);
  let source = "^";
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index]!;
    if (segment === "**") {
      source += index === segments.length - 1 ? ".*" : "(?:[^/]+/)*";
      continue;
    }
    source += globSegmentToRegExpSource(segment);
    if (index < segments.length - 1) {
      source += "/";
    }
  }
  source += "$";
  return new RegExp(source);
}

function globSegmentToRegExpSource(segment: string): string {
  let source = "";
  for (const char of segment) {
    if (char === "*") {
      source += "[^/]*";
    } else if (char === "?") {
      source += "[^/]";
    } else {
      source += escapeRegExp(char);
    }
  }
  return source;
}

function escapeRegExp(value: string): string {
  return value.replace(/[|\\{}()[\]^$+*?.]/g, "\\$&");
}

function matchesAny(relativePath: string, regexes: RegExp[], isDirectory: boolean): boolean {
  return regexes.some(
    (regex) => regex.test(relativePath) || (isDirectory && regex.test(`${relativePath}/x`)),
  );
}

function relativeToRoot(root: string, absolutePath: string): string {
  return path.relative(root, absolutePath).split(path.sep).join("/");
}

function ensureWithinRoot(root: string, absolutePath: string): void {
  const relativePath = path.relative(root, absolutePath);
  if (relativePath.startsWith("..") || path.isAbsolute(relativePath)) {
    throw new Error(`artifact cache scope path is outside root: ${absolutePath}`);
  }
}

async function pathExists(target: string): Promise<boolean> {
  try {
    await access(target, fsConstants.F_OK);
    return true;
  } catch {
    return false;
  }
}

function safePathSegment(value: string): string {
  const readable = value
    .replace(/[^a-zA-Z0-9._-]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 64);
  const hash = createHash("sha256").update(value).digest("hex").slice(0, 12);
  return `${readable || "artifact"}-${hash}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}
