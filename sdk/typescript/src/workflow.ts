import type { CodexOptions } from "./codexOptions";
import { randomUUID } from "node:crypto";
import {
  AppServerClient,
  appServerUrlFromOptions,
  type AppServerClientOptions,
  type AppServerNotification,
  type AppServerRequest,
  type ClientInfo,
} from "./appServerClient";

export type WorkflowUserInput =
  | string
  | Array<
      | { type: "text"; text: string }
      | { type: "image"; url: string }
      | { type: "local_image"; path: string }
      | { type: "skill"; name: string; path: string }
      | { type: "mention"; name: string; path: string }
    >;

export type WorkflowToolSpec = {
  namespace?: string | null;
  name: string;
  description: string;
  inputSchema: unknown;
  deferLoading?: boolean;
};

export type DynamicToolContext = {
  threadId: string;
  turnId: string;
  callId: string;
  namespace?: string | null;
  tool: string;
};

export type DynamicToolResult =
  | string
  | {
      contentItems: DynamicToolOutputContentItem[];
      success?: boolean;
    };

export type DynamicToolOutputContentItem =
  | { type: "inputText"; text: string }
  | { type: "inputImage"; imageUrl: string };

export type DynamicToolHandler = (
  args: unknown,
  context: DynamicToolContext,
) => DynamicToolResult | Promise<DynamicToolResult>;

export type WorkflowTool = WorkflowToolSpec & {
  handler: DynamicToolHandler;
};

export type ArtifactSource = {
  path: string;
  kind: string;
  sha256: string;
};

export type ArtifactState = {
  id: number;
  namespace: string;
  scopeKey: string;
  sourceKey: string;
  stateDir: string;
  metadata: unknown;
  createdAtUnixSec: number;
  updatedAtUnixSec: number;
  lastHitAtUnixSec: number | null;
};

export type ArtifactFile = {
  stateId: number;
  relativePath: string;
  sizeBytes: number;
  sha256: string;
  updatedAtUnixSec: number;
};

export type ArtifactFileMatch = {
  state: ArtifactState;
  file: ArtifactFile;
};

export type ArtifactCacheEntry = {
  namespace: string;
  key: string;
  artifactId: string;
  status: string;
  metadata: unknown;
  createdAtUnixSec: number;
  updatedAtUnixSec: number;
  lastHitAtUnixSec: number | null;
};

export type ArtifactStateRegisterParams = {
  namespace: string;
  scopeKey: string;
  sourceKey: string;
  stateDir: string;
  sources: ArtifactSource[];
  metadata: unknown;
};

export type ArtifactStateRegisterResponse = {
  state: ArtifactState;
};

export type ArtifactStateReadParams = {
  namespace: string;
  scopeKey: string;
  sourceKey: string;
};

export type ArtifactStateReadResponse = {
  state: ArtifactState | null;
};

export type ArtifactStateListParams = {
  namespace: string;
  scopeKey: string;
};

export type ArtifactStateListResponse = {
  states: ArtifactState[];
};

export type ArtifactStateHitParams = {
  namespace: string;
  stateDir: string;
};

export type ArtifactStateHitResponse = Record<string, never>;

export type ArtifactStatePruneParams = {
  namespace: string;
  retentionSecs: number;
  throttleSecs: number;
};

export type ArtifactStatePruneResponse = {
  pruned: number;
};

export type ArtifactFileIndexParams = {
  namespace: string;
  stateDir: string;
  relativePath: string;
};

export type ArtifactFileIndexResponse = {
  file: ArtifactFile;
};

export type ArtifactFileFindParams = {
  namespace: string;
  relativePath: string;
};

export type ArtifactFileFindResponse = {
  entry: ArtifactFileMatch | null;
};

export type ArtifactCacheReadParams = {
  namespace: string;
  key: string;
};

export type ArtifactCacheReadResponse = {
  entry: ArtifactCacheEntry | null;
};

export type ArtifactCacheWriteParams = {
  namespace: string;
  key: string;
  artifactId: string;
  status: string;
  metadata: unknown;
};

export type ArtifactCacheWriteResponse = {
  entry: ArtifactCacheEntry;
};

export type ArtifactCacheDeleteParams = {
  namespace: string;
  key: string;
};

export type ArtifactCacheDeleteResponse = Record<string, never>;

export type WorkflowConnection = "auto" | "require-existing" | "spawn" | { appServerUrl: string };

export type WorkflowApprovalMode = "auto" | "inherit" | "delegate" | "decline";

export type WorkflowApprovalRequest = {
  type: "commandExecution" | "fileChange" | "permissions" | "mcpElicitation";
  method: string;
  id: AppServerRequest["id"];
  params: unknown;
  rawRequest: AppServerRequest;
};

export type WorkflowApprovalResponse = unknown;

export type WorkflowApprovalHandler = (
  request: WorkflowApprovalRequest,
) => WorkflowApprovalResponse | Promise<WorkflowApprovalResponse>;

export type WorkflowApprovals =
  | WorkflowApprovalMode
  | {
      mode: "handler";
      onApproval: WorkflowApprovalHandler;
    };

export type WorkflowInteractiveRequestBehavior = "decline" | "defer";
const WORKFLOW_APPROVALS_ENV = "CODEX_WORKFLOW_APPROVALS";
const WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV = "CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR";
const WORKFLOW_RUN_ID_ENV = "CODEX_WORKFLOW_RUN_ID";
const WORKFLOW_ORIGIN_THREAD_ID_ENV = "CODEX_WORKFLOW_ORIGIN_THREAD_ID";

export type WorkflowOptions = CodexOptions &
  Pick<
    AppServerClientOptions,
    "appServerUrl" | "webSocket" | "webSocketProtocols" | "webSocketOptions" | "experimentalApi"
  > & {
    clientInfo?: ClientInfo;
    /**
     * Controls how the workflow gets an app-server connection. `auto` connects to an explicit or
     * environment-provided app-server URL, otherwise it starts a private stdio app-server.
     */
    connection?: WorkflowConnection;
    /**
     * Controls who answers app-server requests that need a decision, such as command/file approvals
     * and MCP elicitations. This is separate from an agent's `approvalPolicy`, which controls when
     * Codex asks for approval.
     */
    approvals?: WorkflowApprovals;
    /**
     * Controls app-server requests that need a user decision, such as command/file approvals and MCP elicitations.
     * Use `defer` when another client, typically the Codex TUI, is connected to the same app-server and should answer them.
     * @deprecated Use `approvals: "delegate" | "decline"` instead.
     */
    interactiveRequestBehavior?: WorkflowInteractiveRequestBehavior;
  };

export type AgentStartOptions = {
  model?: string;
  modelProvider?: string;
  workingDirectory?: string;
  approvalPolicy?: "never" | "on-request" | "on-failure" | "untrusted";
  sandboxMode?: "read-only" | "workspace-write" | "danger-full-access";
  tools?: WorkflowTool[];
  ephemeral?: boolean;
  developerInstructions?: string;
  baseInstructions?: string;
};

export type AgentResumeOptions = {
  model?: string;
  modelProvider?: string;
  workingDirectory?: string;
  approvalPolicy?: "never" | "on-request" | "on-failure" | "untrusted";
  sandboxMode?: "read-only" | "workspace-write" | "danger-full-access";
  tools?: WorkflowTool[];
  developerInstructions?: string;
  baseInstructions?: string;
};

export type AgentRunOptions = {
  outputSchema?: unknown;
  signal?: AbortSignal;
};

export type SpawnAgentOptions = AgentStartOptions & {
  input?: WorkflowUserInput;
  mode?: "fresh" | "fork";
  /** @deprecated Use `mode: "fork"` instead. */
  fork?: boolean;
};

export type WorkflowTurnResult = {
  threadId: string;
  turn: AppServerTurn | null;
  items: AppServerThreadItem[];
  finalResponse: string;
  usage: unknown;
  status: string | null;
};

export type WorkflowStreamedTurn = {
  events: AsyncGenerator<AppServerNotification>;
};

export type WorkflowProgressEvent = {
  message: string;
  data?: unknown;
};

export type ApiCatalogSection =
  | "appServerMethods"
  | "mcpServers"
  | "builtInTools"
  | "workflowRuntime"
  | "workflows";

export type ApiCatalogReadParams = {
  include?: ApiCatalogSection[] | null;
  mcpDetail?: "full" | "toolsAndAuthOnly" | null;
};

export type ApiCatalogMethod = {
  method: string;
  paramsType: string;
  responseType: string;
  experimental: boolean;
  description: string | null;
};

export type ApiCatalogTool = {
  name: string;
  source: "appServerRpc" | "workflowRuntime";
  invocation: string;
  description: string;
  inputSchema: unknown;
  outputSchema: unknown | null;
};

export type ApiCatalogSymbol = {
  name: string;
  kind: "function" | "class" | "method" | "type";
  signature: string;
  description: string;
};

export type ApiCatalogWorkflowRuntime = {
  package: string;
  importSpecifier: string;
  symbols: ApiCatalogSymbol[];
};

export type ApiCatalogReadResponse = {
  schemaVersion: number;
  generatedAt: number;
  appServerMethods: ApiCatalogMethod[];
  mcpServers: unknown[];
  builtInTools: ApiCatalogTool[];
  workflowRuntime: ApiCatalogWorkflowRuntime;
  workflows: WorkflowSummary[];
};

export type WorkflowRootKind = "global" | "project" | "searchPath";

export type WorkflowValidationStatus = "valid" | "invalid";

export type WorkflowValidationInfo = {
  status: WorkflowValidationStatus;
  messages: string[];
};

export type WorkflowRootInfo = {
  kind: WorkflowRootKind;
  label: string;
  path: string;
};

export type WorkflowSummary = {
  id: string;
  command: string | null;
  title: string | null;
  userDescription: string | null;
  searchTerms: string[];
  rootLabel: string;
  rootKind: WorkflowRootKind;
  rootPath: string;
  path: string;
  workflowYamlPath: string;
  mentionTarget: string;
  validation: WorkflowValidationInfo;
  repairMode: string;
};

export type WorkflowImpactInfo = {
  id: string;
  path: string;
  dependencies: string[];
  devDependencies: string[];
  gitStatus: string[];
};

export type WorkflowConfigValues = {
  search_paths: string[];
  default_location: string;
  repair_mode: string;
  max_repair_cycles: number;
  dependency_update_policy: string;
  commit_policy: string;
  validation_profile: string;
};

export type WorkflowListResponse = {
  roots: WorkflowRootInfo[];
  workflows: WorkflowSummary[];
};

export type WorkflowReadResponse = {
  workflow: WorkflowSummary;
  workflowYaml: string;
  readme: string | null;
};

export type WorkflowImpactResponse = {
  impact: WorkflowImpactInfo;
};

export type WorkflowCommandResponse = {
  message: string;
  data: unknown;
};

export type WorkflowRunStatus = "running" | "succeeded" | "failed" | "canceled";

export type WorkflowRunApprovalHandling = "delegate" | "decline";

export type WorkflowRun = {
  id: string;
  workflowId: string;
  status: WorkflowRunStatus;
  threadId: string | null;
  createdAt: number;
  startedAt: number | null;
  completedAt: number | null;
  output: unknown | null;
  error: string | null;
};

export type WorkflowRunStartParams<Input = unknown> = {
  id: string;
  input?: Input | null;
  threadId?: string | null;
  stageSessionId?: string | null;
  approvalHandling?: WorkflowRunApprovalHandling | null;
};

export type WorkflowRunStartResponse = {
  run: WorkflowRun;
};

export type WorkflowRunReadResponse = {
  run: WorkflowRun;
};

export type WorkflowRunWaitResponse = {
  run: WorkflowRun;
  completed: boolean;
};

export type WorkflowRunCancelResponse = {
  run: WorkflowRun;
};

export type WorkflowConfigReadResponse = {
  config: WorkflowConfigValues;
};

export type WorkflowConfigWriteResponse = WorkflowConfigReadResponse;

export type WorkflowAuthoringContextPrepareResponse = {
  roots: WorkflowRootInfo[];
  workflows: WorkflowSummary[];
  config: WorkflowConfigValues;
};

export type WorkflowRunOptions<Input = unknown> = WorkflowOptions & {
  input?: Input;
  context?: WorkflowContext;
  workflow?: CodexWorkflow;
  onProgress?: (event: WorkflowProgressEvent) => void;
  onReportToUserMarkdown?: (markdown: string) => void;
  onResult?: (result: unknown) => void;
};

export type DefinedWorkflow<Input = unknown, Output = unknown> = {
  name?: string;
  run(context: WorkflowContext, input: Input): Output | Promise<Output>;
};

export type WorkflowContext = {
  readonly workflow: CodexWorkflow;
  readonly api: WorkflowApiCatalog;
  readonly artifacts: WorkflowArtifacts;
  readonly workflows: WorkflowApis;
  readonly mcp: WorkflowMcp;
  readonly tools: WorkflowTools;
  createAgent(options?: AgentStartOptions): Promise<AgentHandle>;
  startAgent(options?: AgentStartOptions): Promise<AgentHandle>;
  resumeAgent(
    threadId: string,
    options?: AgentResumeOptions | WorkflowTool[],
  ): Promise<AgentHandle>;
  runWorkflow<Input = undefined, Output = unknown>(
    workflow: DefinedWorkflow<Input, Output>,
    input?: Input,
  ): Promise<Output>;
  progress(message: string, data?: unknown): void;
  reportToUserMarkdown(markdown: string): void;
  result(result: unknown): void;
};

export type WorkflowToolRegistration = {
  dispose(): void;
};

type ResolvedWorkflowConnection = { kind: "existing"; appServerUrl: string } | { kind: "spawn" };

type ResolvedWorkflowApprovals =
  | "delegate"
  | "decline"
  | {
      mode: "handler";
      onApproval: WorkflowApprovalHandler;
    };

type AppServerThread = {
  id: string;
  turns?: AppServerTurn[];
};

type AppServerTurn = {
  id: string;
  status?: string;
  items?: AppServerThreadItem[];
};

type AppServerThreadItem = {
  type?: string;
  text?: string;
  [key: string]: unknown;
};

type ThreadStartResponse = {
  thread: AppServerThread;
};

type ThreadForkResponse = {
  thread: AppServerThread;
};

type ThreadResumeResponse = {
  thread: AppServerThread;
};

type TurnStartResponse = {
  turn: AppServerTurn;
};

type WorkflowNotificationContext = {
  runId: string;
  originThreadId: string | null;
};

type TurnCompletedParams = {
  threadId: string;
  turn: AppServerTurn;
};

type ItemCompletedParams = {
  threadId: string;
  turnId: string;
  item: AppServerThreadItem;
};

type DynamicToolCallParams = {
  threadId: string;
  turnId: string;
  callId: string;
  namespace?: string | null;
  tool: string;
  arguments: unknown;
};

type MappedUserInput =
  | { type: "text"; text: string; text_elements: [] }
  | { type: "image"; url: string }
  | { type: "localImage"; path: string }
  | { type: "skill"; name: string; path: string }
  | { type: "mention"; name: string; path: string };

export function defineTool(spec: WorkflowToolSpec, handler: DynamicToolHandler): WorkflowTool {
  return { ...spec, handler };
}

export function defineWorkflow<Input = unknown, Output = unknown>(
  workflow: DefinedWorkflow<Input, Output>,
): DefinedWorkflow<Input, Output> {
  return workflow;
}

export async function runWorkflow<Input = undefined, Output = unknown>(
  definition: DefinedWorkflow<Input, Output>,
  options: WorkflowRunOptions<Input> = {},
): Promise<Output> {
  if (options.context) {
    return definition.run(options.context, options.input as Input);
  }

  let ownsWorkflow = false;
  const workflow = options.workflow ?? (await CodexWorkflow.start(options));
  ownsWorkflow = options.workflow === undefined;
  const context = new DefaultWorkflowContext(workflow, {
    onProgress: options.onProgress,
    onReportToUserMarkdown: options.onReportToUserMarkdown,
    onResult: options.onResult,
  });

  try {
    return await definition.run(context, options.input as Input);
  } finally {
    if (ownsWorkflow) {
      await workflow.close();
    }
  }
}

export class CodexWorkflow {
  readonly api: WorkflowApiCatalog;
  readonly artifacts: WorkflowArtifacts;
  readonly workflows: WorkflowApis;
  readonly mcp: WorkflowMcp;
  readonly tools: WorkflowTools;

  private client: AppServerClient;
  private readonly notificationContext: WorkflowNotificationContext;
  private agents = new Map<string, AgentHandle>();
  private dynamicTools = new Map<string, WorkflowTool>();
  private approvals: ResolvedWorkflowApprovals;

  private constructor(
    client: AppServerClient,
    approvals: ResolvedWorkflowApprovals,
    notificationContext: WorkflowNotificationContext,
  ) {
    this.client = client;
    this.approvals = approvals;
    this.notificationContext = notificationContext;
    this.api = new WorkflowApiCatalog(client);
    this.artifacts = new WorkflowArtifacts(client);
    this.workflows = new WorkflowApis(client);
    this.mcp = new WorkflowMcp(client);
    this.tools = new WorkflowTools(client);
    this.client.onServerRequest((request) => this.handleServerRequest(request));
  }

  static async start(options: WorkflowOptions = {}): Promise<CodexWorkflow> {
    const connection = resolveWorkflowConnection(options);
    const approvals = resolveWorkflowApprovals(options);
    const notificationContext = workflowNotificationContextFromEnv();
    const client =
      connection.kind === "existing"
        ? await AppServerClient.connect({ ...options, appServerUrl: connection.appServerUrl })
        : await AppServerClient.spawn(options);
    return new CodexWorkflow(client, approvals, notificationContext);
  }

  static async connect(options: WorkflowOptions = {}): Promise<CodexWorkflow> {
    return this.start({ ...options, connection: "require-existing" });
  }

  static async spawnServer(options: WorkflowOptions = {}): Promise<CodexWorkflow> {
    return this.start({ ...options, connection: "spawn" });
  }

  static async fromTui(options: WorkflowOptions = {}): Promise<CodexWorkflow> {
    return this.start({
      ...options,
      approvals: options.approvals ?? "delegate",
      connection: "require-existing",
    });
  }

  async close(): Promise<void> {
    await this.client.close();
    this.agents.clear();
  }

  /** @internal */
  notifyWorkflowProgress(message: string, data?: unknown): void {
    this.client.notify("workflowRun/progress", {
      runId: this.notificationContext.runId,
      threadId: this.notificationContext.originThreadId ?? undefined,
      message,
      data,
    });
  }

  /** @internal */
  notifyWorkflowMarkdown(markdown: string): void {
    this.client.notify("workflowRun/reportToUserMarkdown", {
      runId: this.notificationContext.runId,
      threadId: this.notificationContext.originThreadId ?? undefined,
      markdown,
    });
  }

  async startAgent(options: AgentStartOptions = {}): Promise<AgentHandle> {
    this.registerTools(options.tools ?? []);
    const response = await this.client.request<ThreadStartResponse>("thread/start", {
      model: options.model,
      modelProvider: options.modelProvider,
      cwd: options.workingDirectory,
      approvalPolicy: options.approvalPolicy,
      sandbox: options.sandboxMode ? sandboxModeToWire(options.sandboxMode) : undefined,
      dynamicTools: (options.tools ?? []).map(toolSpecToWire),
      ephemeral: options.ephemeral,
      developerInstructions: options.developerInstructions,
      baseInstructions: options.baseInstructions,
    });
    return this.trackAgent(response.thread.id, options.tools ?? []);
  }

  async createAgent(options: AgentStartOptions = {}): Promise<AgentHandle> {
    return this.startAgent(options);
  }

  async resumeAgent(
    threadId: string,
    options: AgentResumeOptions | WorkflowTool[] = {},
  ): Promise<AgentHandle> {
    const resumeOptions = Array.isArray(options) ? { tools: options } : options;
    this.registerTools(resumeOptions.tools ?? []);
    const response = await this.client.request<ThreadResumeResponse>("thread/resume", {
      threadId,
      model: resumeOptions.model,
      modelProvider: resumeOptions.modelProvider,
      cwd: resumeOptions.workingDirectory,
      approvalPolicy: resumeOptions.approvalPolicy,
      sandbox: resumeOptions.sandboxMode ? sandboxModeToWire(resumeOptions.sandboxMode) : undefined,
      developerInstructions: resumeOptions.developerInstructions,
      baseInstructions: resumeOptions.baseInstructions,
    });
    return this.trackAgent(response.thread.id, resumeOptions.tools ?? []);
  }

  listAgents(): AgentHandle[] {
    return Array.from(this.agents.values());
  }

  registerTools(tools: WorkflowTool[]): WorkflowToolRegistration {
    const registered: Array<{ key: string; tool: WorkflowTool }> = [];
    for (const tool of tools) {
      const key = toolKey(tool.namespace, tool.name);
      this.dynamicTools.set(key, tool);
      registered.push({ key, tool });
    }
    return {
      dispose: () => {
        for (const { key, tool } of registered) {
          if (this.dynamicTools.get(key) === tool) {
            this.dynamicTools.delete(key);
          }
        }
      },
    };
  }

  /** @internal */
  trackAgent(threadId: string, tools: WorkflowTool[]): AgentHandle {
    const agent = new AgentHandle(this, this.client, threadId, tools);
    this.agents.set(threadId, agent);
    return agent;
  }

  /** @internal */
  removeAgent(threadId: string): void {
    this.agents.delete(threadId);
  }

  private async handleServerRequest(request: AppServerRequest): Promise<boolean> {
    if (request.method === "item/tool/call") {
      await this.handleDynamicToolCall(request);
      return true;
    }
    if (isInteractiveRequest(request.method)) {
      await this.handleInteractiveRequest(request);
      return true;
    }
    return false;
  }

  private async handleInteractiveRequest(request: AppServerRequest): Promise<void> {
    if (this.approvals === "delegate") {
      return;
    }

    if (this.approvals === "decline") {
      declineInteractiveRequest(this.client, request);
      return;
    }

    try {
      const response = await this.approvals.onApproval(toWorkflowApprovalRequest(request));
      if (response === undefined) {
        throw new Error("workflow approval handler returned undefined");
      }
      this.client.respond(request.id, response);
    } catch (error) {
      this.client.reject(request.id, {
        code: -32603,
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }

  private async handleDynamicToolCall(request: AppServerRequest): Promise<void> {
    const params = request.params as DynamicToolCallParams;
    const tool = this.dynamicTools.get(toolKey(params.namespace, params.tool));
    if (!tool) {
      this.client.respond(request.id, {
        contentItems: [
          { type: "inputText", text: `No JavaScript handler registered for ${params.tool}` },
        ],
        success: false,
      });
      return;
    }

    try {
      const result = await tool.handler(params.arguments, {
        threadId: params.threadId,
        turnId: params.turnId,
        callId: params.callId,
        namespace: params.namespace,
        tool: params.tool,
      });
      this.client.respond(request.id, normalizeToolResult(result));
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      this.client.respond(request.id, {
        contentItems: [{ type: "inputText", text: message }],
        success: false,
      });
    }
  }
}

export class AgentHandle {
  private workflow: CodexWorkflow;
  private client: AppServerClient;
  private activeRun: Promise<WorkflowTurnResult> | null = null;
  private tools: WorkflowTool[];

  /** @internal */
  constructor(
    workflow: CodexWorkflow,
    client: AppServerClient,
    readonly threadId: string,
    tools: WorkflowTool[] = [],
  ) {
    this.workflow = workflow;
    this.client = client;
    this.tools = tools;
  }

  async run(input: WorkflowUserInput, options: AgentRunOptions = {}): Promise<WorkflowTurnResult> {
    const streamed = await this.runStreamed(input, options);
    const result = collectTurn(this.threadId, streamed.events);
    this.activeRun = result;
    return result;
  }

  async runStreamed(
    input: WorkflowUserInput,
    options: AgentRunOptions = {},
  ): Promise<WorkflowStreamedTurn> {
    const stream = createTurnEventStream(this.client, this.threadId, options.signal);
    try {
      const response = await this.client.request<TurnStartResponse>("turn/start", {
        threadId: this.threadId,
        input: normalizeInput(input),
        outputSchema: options.outputSchema,
      });
      stream.setTurnId(response.turn.id);
      return { events: stream.events };
    } catch (error) {
      stream.close();
      throw error;
    }
  }

  async spawnAgent(options: SpawnAgentOptions = {}): Promise<AgentHandle> {
    const tools = options.tools ?? this.tools;
    this.workflow.registerTools(tools);
    const mode = options.mode ?? (options.fork === true ? "fork" : "fresh");
    const agent =
      mode === "fork"
        ? await this.fork({ ...options, tools })
        : await this.workflow.startAgent({ ...options, tools });
    if (options.input !== undefined) {
      agent.activeRun = agent.run(options.input);
    }
    return agent;
  }

  async createAgent(options: SpawnAgentOptions = {}): Promise<AgentHandle> {
    return this.spawnAgent(options);
  }

  async fork(options: AgentStartOptions = {}): Promise<AgentHandle> {
    this.workflow.registerTools(options.tools ?? []);
    const response = await this.client.request<ThreadForkResponse>("thread/fork", {
      threadId: this.threadId,
      cwd: options.workingDirectory,
      model: options.model,
      modelProvider: options.modelProvider,
      approvalPolicy: options.approvalPolicy,
      sandbox: options.sandboxMode ? sandboxModeToWire(options.sandboxMode) : undefined,
      ephemeral: options.ephemeral,
    });
    return this.workflow.trackAgent(response.thread.id, options.tools ?? this.tools);
  }

  async sendInput(
    input: WorkflowUserInput,
    options: AgentRunOptions = {},
  ): Promise<WorkflowTurnResult> {
    return this.run(input, options);
  }

  async wait(): Promise<WorkflowTurnResult | null> {
    return this.activeRun;
  }

  async unsubscribe(): Promise<void> {
    await this.client.request("thread/unsubscribe", { threadId: this.threadId });
    this.workflow.removeAgent(this.threadId);
  }

  async close(): Promise<void> {
    await this.unsubscribe();
  }
}

export class WorkflowMcp {
  constructor(private client: AppServerClient) {}

  listServers(
    params: { cursor?: string; limit?: number; detail?: "full" | "toolsAndAuthOnly" } = {},
  ) {
    return this.client.request("mcpServerStatus/list", params);
  }

  readResource(params: { threadId?: string; server: string; uri: string }) {
    return this.client.request("mcpServer/resource/read", params);
  }

  callTool(
    agentOrThreadId: AgentHandle | string,
    params: { server: string; tool: string; arguments?: unknown; meta?: unknown },
  ) {
    const threadId =
      typeof agentOrThreadId === "string" ? agentOrThreadId : agentOrThreadId.threadId;
    return this.client.request("mcpServer/tool/call", {
      threadId,
      server: params.server,
      tool: params.tool,
      arguments: params.arguments,
      _meta: params.meta,
    });
  }
}

export class WorkflowArtifacts {
  constructor(private client: AppServerClient) {}

  registerState(params: ArtifactStateRegisterParams): Promise<ArtifactStateRegisterResponse> {
    return this.client.request("artifact/state/register", params);
  }

  readState(params: ArtifactStateReadParams): Promise<ArtifactStateReadResponse> {
    return this.client.request("artifact/state/read", params);
  }

  listStates(params: ArtifactStateListParams): Promise<ArtifactStateListResponse> {
    return this.client.request("artifact/state/list", params);
  }

  recordStateHit(params: ArtifactStateHitParams): Promise<ArtifactStateHitResponse> {
    return this.client.request("artifact/state/hit", params);
  }

  pruneStates(params: ArtifactStatePruneParams): Promise<ArtifactStatePruneResponse> {
    return this.client.request("artifact/state/prune", params);
  }

  indexFile(params: ArtifactFileIndexParams): Promise<ArtifactFileIndexResponse> {
    return this.client.request("artifact/file/index", params);
  }

  findFile(params: ArtifactFileFindParams): Promise<ArtifactFileFindResponse> {
    return this.client.request("artifact/file/find", params);
  }

  readCacheEntry(params: ArtifactCacheReadParams): Promise<ArtifactCacheReadResponse> {
    return this.client.request("artifact/cache/read", params);
  }

  writeCacheEntry(params: ArtifactCacheWriteParams): Promise<ArtifactCacheWriteResponse> {
    return this.client.request("artifact/cache/write", params);
  }

  deleteCacheEntry(params: ArtifactCacheDeleteParams): Promise<ArtifactCacheDeleteResponse> {
    return this.client.request("artifact/cache/delete", params);
  }
}

export class WorkflowApiCatalog {
  constructor(private client: AppServerClient) {}

  read(params: ApiCatalogReadParams = {}): Promise<ApiCatalogReadResponse> {
    return this.client.request("apiCatalog/read", params);
  }
}

export class WorkflowApis {
  readonly registry: WorkflowRegistryApi;
  readonly config: WorkflowConfigApi;
  readonly command: WorkflowCommandApi;

  constructor(private client: AppServerClient) {
    this.registry = new WorkflowRegistryApi(client);
    this.config = new WorkflowConfigApi(client);
    this.command = new WorkflowCommandApi(client);
  }

  run(id: string, input?: unknown): Promise<WorkflowCommandResponse> {
    return this.start({ id, input }).then(async ({ run }) => {
      const waited = await this.wait(run.id);
      if (waited.run.status === "failed") {
        throw new Error(waited.run.error ?? `workflow ${id} failed`);
      }
      if (waited.run.status === "canceled") {
        throw new Error(waited.run.error ?? `workflow ${id} was canceled`);
      }
      return {
        message: JSON.stringify(waited.run.output ?? null, null, 2),
        data: waited.run.output,
      };
    });
  }

  start<Input = unknown>(
    params: WorkflowRunStartParams<Input>,
  ): Promise<WorkflowRunStartResponse> {
    return this.client.request("workflowRun/start", params);
  }

  read(runId: string): Promise<WorkflowRunReadResponse> {
    return this.client.request("workflowRun/read", { runId });
  }

  wait(runId: string, timeoutMs?: number | null): Promise<WorkflowRunWaitResponse> {
    return this.client.request("workflowRun/wait", { runId, timeoutMs });
  }

  cancel(runId: string): Promise<WorkflowRunCancelResponse> {
    return this.client.request("workflowRun/cancel", { runId });
  }
}

export class WorkflowRegistryApi {
  constructor(private client: AppServerClient) {}

  list(): Promise<WorkflowListResponse> {
    return this.client.request("workflow/list", {});
  }

  read(id: string, target?: string | null): Promise<WorkflowReadResponse> {
    return this.client.request("workflow/read", { id, target });
  }

  impact(id: string): Promise<WorkflowImpactResponse> {
    return this.client.request("workflow/impact", { id });
  }

  develop(description: string): Promise<WorkflowCommandResponse> {
    return this.client.request("workflow/develop", { description });
  }

  edit(id: string, instruction: string): Promise<WorkflowCommandResponse> {
    return this.client.request("workflow/edit", { id, instruction });
  }

  validate(id: string): Promise<WorkflowCommandResponse> {
    return this.client.request("workflow/validate", { id });
  }

  repair(id: string): Promise<WorkflowCommandResponse> {
    return this.client.request("workflow/repair", { id });
  }

  authoringContextPrepare(
    params: { id?: string | null; description?: string | null } = {},
  ): Promise<WorkflowAuthoringContextPrepareResponse> {
    return this.client.request("workflow/authoringContext/prepare", params);
  }
}

export class WorkflowConfigApi {
  constructor(private client: AppServerClient) {}

  read(): Promise<WorkflowConfigReadResponse> {
    return this.client.request("workflow/config/read", {});
  }

  write(key: string, value?: unknown): Promise<WorkflowConfigWriteResponse> {
    return this.client.request("workflow/config/write", { key, value });
  }
}

export class WorkflowCommandApi {
  constructor(private client: AppServerClient) {}

  execute(args: string[]): Promise<WorkflowCommandResponse> {
    return this.client.request("workflow/command/execute", { args });
  }
}

export class WorkflowTools {
  constructor(private client: AppServerClient) {}

  exec(command: string[], options: Record<string, unknown> = {}) {
    return this.client.request("command/exec", { command, ...options });
  }
}

class DefaultWorkflowContext implements WorkflowContext {
  readonly api: WorkflowApiCatalog;
  readonly artifacts: WorkflowArtifacts;
  readonly workflows: WorkflowApis;
  readonly mcp: WorkflowMcp;
  readonly tools: WorkflowTools;

  constructor(
    readonly workflow: CodexWorkflow,
    private hooks: {
      onProgress?: (event: WorkflowProgressEvent) => void;
      onReportToUserMarkdown?: (markdown: string) => void;
      onResult?: (result: unknown) => void;
    } = {},
  ) {
    this.api = workflow.api;
    this.artifacts = workflow.artifacts;
    this.workflows = workflow.workflows;
    this.mcp = workflow.mcp;
    this.tools = workflow.tools;
  }

  createAgent(options: AgentStartOptions = {}): Promise<AgentHandle> {
    return this.workflow.createAgent(options);
  }

  startAgent(options: AgentStartOptions = {}): Promise<AgentHandle> {
    return this.workflow.startAgent(options);
  }

  resumeAgent(
    threadId: string,
    options: AgentResumeOptions | WorkflowTool[] = {},
  ): Promise<AgentHandle> {
    return this.workflow.resumeAgent(threadId, options);
  }

  runWorkflow<Input = undefined, Output = unknown>(
    workflow: DefinedWorkflow<Input, Output>,
    input?: Input,
  ): Promise<Output> {
    return Promise.resolve(workflow.run(this, input as Input));
  }

  progress(message: string, data?: unknown): void {
    this.hooks.onProgress?.({ message, data });
    this.workflow.notifyWorkflowProgress(message, data);
  }

  reportToUserMarkdown(markdown: string): void {
    this.hooks.onReportToUserMarkdown?.(markdown);
    this.workflow.notifyWorkflowMarkdown(markdown);
  }

  result(result: unknown): void {
    this.hooks.onResult?.(result);
  }
}

export type { WebSocketConstructor, WebSocketLike } from "./appServerClient";

type TurnEventStream = {
  events: AsyncGenerator<AppServerNotification>;
  setTurnId: (turnId: string) => void;
  close: () => void;
};

function createTurnEventStream(
  client: AppServerClient,
  threadId: string,
  signal?: AbortSignal,
): TurnEventStream {
  const queue: AppServerNotification[] = [];
  let turnId: string | null = null;
  let wake: (() => void) | null = null;
  let done = false;
  let closed = false;
  let offNotification: () => void = () => undefined;

  const notifyReader = () => {
    wake?.();
    wake = null;
  };
  const interrupt = () => {
    if (turnId) {
      void client.request("turn/interrupt", { threadId, turnId }).catch(() => undefined);
    }
  };
  const onAbort = () => interrupt();
  const cleanup = () => {
    if (closed) {
      return;
    }
    closed = true;
    offNotification();
    signal?.removeEventListener("abort", onAbort);
    notifyReader();
  };

  offNotification = client.onNotification((notification) => {
    if (!isThreadNotification(notification, threadId)) {
      return;
    }
    if (turnId && !isTurnNotification(notification, threadId, turnId)) {
      return;
    }
    queue.push(notification);
    if (turnId && notification.method === "turn/completed") {
      done = true;
      cleanup();
    }
    notifyReader();
  });
  signal?.addEventListener("abort", onAbort, { once: true });

  const events = (async function* () {
    try {
      while (!done || queue.length > 0) {
        if (turnId === null) {
          if (done) {
            break;
          }
          await new Promise<void>((resolve) => {
            wake = resolve;
          });
          continue;
        }
        if (queue.length === 0) {
          await new Promise<void>((resolve) => {
            wake = resolve;
          });
        }
        while (turnId !== null && queue.length > 0) {
          const notification = queue.shift();
          if (!notification || !isTurnNotification(notification, threadId, turnId)) {
            continue;
          }
          if (notification.method === "turn/completed") {
            done = true;
          }
          yield notification;
        }
      }
    } finally {
      cleanup();
    }
  })();

  return {
    events,
    setTurnId(nextTurnId: string) {
      turnId = nextTurnId;
      if (
        queue.some(
          (notification) =>
            notification.method === "turn/completed" &&
            isTurnNotification(notification, threadId, nextTurnId),
        )
      ) {
        done = true;
        cleanup();
      }
      if (signal?.aborted) {
        interrupt();
      }
      notifyReader();
    },
    close() {
      done = true;
      cleanup();
    },
  };
}

async function collectTurn(
  threadId: string,
  events: AsyncGenerator<AppServerNotification>,
): Promise<WorkflowTurnResult> {
  const items: AppServerThreadItem[] = [];
  let finalResponse = "";
  let usage: unknown = null;
  let turn: AppServerTurn | null = null;
  let status: string | null = null;

  for await (const event of events) {
    if (event.method === "item/completed") {
      const params = event.params as ItemCompletedParams;
      items.push(params.item);
      if (params.item.type === "agentMessage" || params.item.type === "agent_message") {
        finalResponse = typeof params.item.text === "string" ? params.item.text : finalResponse;
      }
    } else if (event.method === "turn/completed") {
      const params = event.params as TurnCompletedParams & { usage?: unknown };
      turn = params.turn;
      status = params.turn.status ?? null;
      usage = params.usage ?? null;
      if (Array.isArray(params.turn.items)) {
        for (const item of params.turn.items) {
          if (item.type === "agentMessage" && typeof item.text === "string") {
            finalResponse = item.text;
          }
        }
      }
    }
  }

  return { threadId, turn, items, finalResponse, usage, status };
}

function isTurnNotification(
  notification: AppServerNotification,
  threadId: string,
  turnId: string,
): boolean {
  const params = notification.params as
    | { threadId?: string; turnId?: string; turn?: { id?: string } }
    | undefined;
  if (!params || params.threadId !== threadId) {
    return false;
  }
  return params.turnId === turnId || params.turn?.id === turnId;
}

function isThreadNotification(notification: AppServerNotification, threadId: string): boolean {
  const params = notification.params as { threadId?: string } | undefined;
  return params?.threadId === threadId;
}

function resolveWorkflowConnection(options: WorkflowOptions): ResolvedWorkflowConnection {
  const connection = options.connection ?? "auto";
  if (typeof connection === "object") {
    return { kind: "existing", appServerUrl: connection.appServerUrl };
  }

  if (connection === "spawn") {
    return { kind: "spawn" };
  }

  const appServerUrl = appServerUrlFromOptions(options);
  if (appServerUrl) {
    return { kind: "existing", appServerUrl };
  }

  if (connection === "require-existing") {
    throw new Error(
      "No Codex app-server URL is available. Set appServerUrl, CODEX_APP_SERVER_URL, or CODEX_WORKFLOW_APP_SERVER_URL.",
    );
  }

  return { kind: "spawn" };
}

function resolveWorkflowApprovals(options: WorkflowOptions): ResolvedWorkflowApprovals {
  if (options.approvals && options.approvals !== "auto") {
    if (options.approvals === "inherit") {
      throw new Error('approvals: "inherit" requires an existing WorkflowContext');
    }
    return options.approvals;
  }

  const explicitLegacyBehavior = options.interactiveRequestBehavior;
  if (explicitLegacyBehavior) {
    return interactiveRequestBehaviorToApprovals(explicitLegacyBehavior);
  }

  const envApprovals = workflowApprovalsFromEnv(options);
  if (envApprovals) {
    return envApprovals;
  }

  const legacyEnvBehavior = interactiveRequestBehaviorFromEnv(options);
  if (legacyEnvBehavior) {
    return interactiveRequestBehaviorToApprovals(legacyEnvBehavior);
  }

  return "decline";
}

function workflowNotificationContextFromEnv(env = process.env): WorkflowNotificationContext {
  const runId = env[WORKFLOW_RUN_ID_ENV];
  return {
    runId: runId && runId.length > 0 ? runId : randomUUID(),
    originThreadId: env[WORKFLOW_ORIGIN_THREAD_ID_ENV] || null,
  };
}

function workflowApprovalsFromEnv(options: WorkflowOptions): "delegate" | "decline" | undefined {
  const env = options.env ?? process.env;
  const value = env[WORKFLOW_APPROVALS_ENV];
  if (value === "delegate" || value === "decline") {
    return value;
  }
  return undefined;
}

function interactiveRequestBehaviorFromEnv(
  options: WorkflowOptions,
): WorkflowInteractiveRequestBehavior | undefined {
  const env = options.env ?? process.env;
  const value = env[WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR_ENV];
  if (value === "decline" || value === "defer") {
    return value;
  }
  return undefined;
}

function interactiveRequestBehaviorToApprovals(
  behavior: WorkflowInteractiveRequestBehavior,
): "delegate" | "decline" {
  return behavior === "defer" ? "delegate" : "decline";
}

function declineInteractiveRequest(client: AppServerClient, request: AppServerRequest): void {
  if (request.method === "item/commandExecution/requestApproval") {
    client.respond(request.id, { decision: "decline" });
    return;
  }
  if (request.method === "item/fileChange/requestApproval") {
    client.respond(request.id, { decision: "decline" });
    return;
  }
  if (request.method === "mcpServer/elicitation/request") {
    client.respond(request.id, { action: "decline", content: null, _meta: null });
    return;
  }
  if (request.method === "item/permissions/requestApproval") {
    client.respond(request.id, { permissions: {}, scope: "turn" });
  }
}

function toWorkflowApprovalRequest(request: AppServerRequest): WorkflowApprovalRequest {
  const type = approvalRequestType(request.method);
  if (!type) {
    throw new Error(`Unsupported approval request method ${request.method}`);
  }
  return {
    type,
    method: request.method,
    id: request.id,
    params: request.params,
    rawRequest: request,
  };
}

function approvalRequestType(method: string): WorkflowApprovalRequest["type"] | null {
  switch (method) {
    case "item/commandExecution/requestApproval":
      return "commandExecution";
    case "item/fileChange/requestApproval":
      return "fileChange";
    case "item/permissions/requestApproval":
      return "permissions";
    case "mcpServer/elicitation/request":
      return "mcpElicitation";
    default:
      return null;
  }
}

function normalizeInput(input: WorkflowUserInput): MappedUserInput[] {
  if (typeof input === "string") {
    return [{ type: "text", text: input, text_elements: [] }];
  }
  return input.map((item) => {
    switch (item.type) {
      case "text":
        return { type: "text", text: item.text, text_elements: [] };
      case "image":
        return { type: "image", url: item.url };
      case "local_image":
        return { type: "localImage", path: item.path };
      case "skill":
        return { type: "skill", name: item.name, path: item.path };
      case "mention":
        return { type: "mention", name: item.name, path: item.path };
    }
  });
}

function toolSpecToWire(tool: WorkflowTool): Record<string, unknown> {
  return {
    namespace: tool.namespace ?? undefined,
    name: tool.name,
    description: tool.description,
    inputSchema: tool.inputSchema,
    deferLoading: tool.deferLoading ?? false,
  };
}

function normalizeToolResult(result: DynamicToolResult): {
  contentItems: DynamicToolOutputContentItem[];
  success: boolean;
} {
  if (typeof result === "string") {
    return { contentItems: [{ type: "inputText", text: result }], success: true };
  }
  return { contentItems: result.contentItems, success: result.success ?? true };
}

function toolKey(namespace: string | null | undefined, name: string): string {
  return `${namespace ?? ""}\u0000${name}`;
}

function sandboxModeToWire(mode: NonNullable<AgentStartOptions["sandboxMode"]>): string {
  return mode;
}

function isInteractiveRequest(method: string): boolean {
  return approvalRequestType(method) !== null;
}
