import * as child_process from "node:child_process";
import { createHash } from "node:crypto";
import { EventEmitter } from "node:events";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer, type Socket } from "node:net";
import { PassThrough } from "node:stream";

import { beforeEach, describe, expect, it } from "@jest/globals";

import {
  CodexWorkflow,
  defineTool,
  defineWorkflow,
  runWorkflow,
  type WebSocketConstructor,
} from "../src/workflow";

jest.mock("node:child_process", () => {
  const actual = jest.requireActual<typeof import("node:child_process")>("node:child_process");
  return { ...actual, spawn: jest.fn() };
});

type ActualChildProcess = typeof import("node:child_process");
const spawnMock = child_process.spawn as jest.MockedFunction<ActualChildProcess["spawn"]>;

type JsonMessage = Record<string, unknown>;

class FakeAppServerProcess extends EventEmitter {
  stdin = new PassThrough();
  stdout = new PassThrough();
  stderr = new PassThrough();
  killed = false;
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  threadResumeParams: JsonMessage | null = null;
  threadStartParams: JsonMessage | null = null;
  turnStartParams: JsonMessage | null = null;
  toolResponse: JsonMessage | null = null;
  approvalResponse: JsonMessage | null = null;
  apiCatalogReadParams: JsonMessage | null = null;
  workflowRunParams: JsonMessage | null = null;
  workflowRunWaitParams: JsonMessage | null = null;
  workflowCommandExecuteParams: JsonMessage | null = null;
  workflowNotifications: JsonMessage[] = [];
  artifactRequests: Array<{ method: string; params: JsonMessage }> = [];
  sendApprovalRequestOnThreadStart = false;

  private buffer = "";
  private startCount = 0;
  private threadId = "thread-1";
  private turnId = "turn-1";
  private artifactState: JsonMessage | null = null;
  private artifactFile: JsonMessage | null = null;
  private artifactCacheEntry: JsonMessage | null = null;

  constructor() {
    super();
    this.stdin.on("data", (chunk: Buffer | string) => {
      this.buffer += chunk.toString();
      this.drainLines();
    });
  }

  kill(): boolean {
    this.killed = true;
    this.exitCode = 0;
    this.emit("exit", 0, null);
    this.stdout.end();
    this.stderr.end();
    return true;
  }

  private drainLines(): void {
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline === -1) {
        return;
      }
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (line.trim()) {
        this.handleMessage(JSON.parse(line) as JsonMessage);
      }
    }
  }

  private handleMessage(message: JsonMessage): void {
    if (message.method === "initialize") {
      this.write({ id: message.id, result: { userAgent: "fake", codexHome: "/tmp/codex" } });
      return;
    }
    if (message.method === "initialized") {
      return;
    }
    if (message.method === "thread/start") {
      this.startCount += 1;
      this.threadId = `thread-${this.startCount}`;
      this.threadStartParams = message.params as JsonMessage;
      this.write({ id: message.id, result: { thread: { id: this.threadId, turns: [] } } });
      if (this.sendApprovalRequestOnThreadStart) {
        setImmediate(() => {
          this.write({
            id: "approval-request-1",
            method: "item/commandExecution/requestApproval",
            params: {
              threadId: this.threadId,
              turnId: this.turnId,
              itemId: "call-1",
              approvalId: "approval-1",
              command: "echo hi",
            },
          });
        });
      }
      return;
    }
    if (message.method === "thread/resume") {
      this.threadResumeParams = message.params as JsonMessage;
      this.threadId = message.params
        ? String((message.params as JsonMessage).threadId)
        : "resumed-thread";
      this.write({ id: message.id, result: { thread: { id: this.threadId, turns: [] } } });
      return;
    }
    if (message.method === "thread/fork") {
      this.threadId = "thread-forked";
      this.write({ id: message.id, result: { thread: { id: this.threadId, turns: [] } } });
      return;
    }
    if (message.method === "turn/start") {
      this.turnStartParams = message.params as JsonMessage;
      this.write({
        id: message.id,
        result: { turn: { id: this.turnId, status: "inProgress", items: [] } },
      });
      setImmediate(() => {
        this.write({
          id: "tool-request-1",
          method: "item/tool/call",
          params: {
            threadId: this.threadId,
            turnId: this.turnId,
            callId: "call-1",
            namespace: "js",
            tool: "lookup_weather",
            arguments: { city: "Paris" },
          },
        });
      });
      return;
    }
    if (message.id === "tool-request-1") {
      this.toolResponse = message.result as JsonMessage;
      const agentItem = { id: "item-agent", type: "agentMessage", text: "Weather: mild" };
      this.write({
        method: "item/completed",
        params: {
          threadId: this.threadId,
          turnId: this.turnId,
          item: { id: "item-tool", type: "dynamicToolCall", status: "completed" },
        },
      });
      this.write({
        method: "item/completed",
        params: { threadId: this.threadId, turnId: this.turnId, item: agentItem },
      });
      this.write({
        method: "turn/completed",
        params: {
          threadId: this.threadId,
          turn: { id: this.turnId, status: "completed", items: [agentItem] },
        },
      });
      return;
    }
    if (message.id === "approval-request-1") {
      this.approvalResponse = message;
      return;
    }
    if (message.method === "mcpServer/tool/call") {
      this.write({ id: message.id, result: { content: [{ type: "text", text: "ok" }] } });
      return;
    }
    if (message.method === "command/exec") {
      this.write({ id: message.id, result: { exitCode: 0, stdout: "done", stderr: "" } });
      return;
    }
    if (typeof message.method === "string" && message.method.startsWith("artifact/")) {
      this.artifactRequests.push({ method: message.method, params: message.params as JsonMessage });
      if (message.method === "artifact/state/register") {
        const params = message.params as JsonMessage;
        this.artifactState = {
          id: 1,
          namespace: String(params.namespace),
          scopeKey: String(params.scopeKey),
          sourceKey: String(params.sourceKey),
          stateDir: String(params.stateDir),
          metadata: params.metadata,
          createdAtUnixSec: 123,
          updatedAtUnixSec: 123,
          lastHitAtUnixSec: null,
        };
        this.write({ id: message.id, result: { state: this.artifactState } });
        return;
      }
      if (message.method === "artifact/state/read") {
        this.write({ id: message.id, result: { state: this.artifactState } });
        return;
      }
      if (message.method === "artifact/state/list") {
        this.write({ id: message.id, result: { states: this.artifactState ? [this.artifactState] : [] } });
        return;
      }
      if (message.method === "artifact/state/hit") {
        if (this.artifactState) {
          this.artifactState = { ...this.artifactState, lastHitAtUnixSec: 124, updatedAtUnixSec: 124 };
        }
        this.write({ id: message.id, result: {} });
        return;
      }
      if (message.method === "artifact/state/prune") {
        this.write({ id: message.id, result: { pruned: 0 } });
        return;
      }
      if (message.method === "artifact/file/index") {
        const params = message.params as JsonMessage;
        this.artifactFile = {
          stateId: 1,
          relativePath: String(params.relativePath),
          sizeBytes: 18,
          sha256: "abc123",
          updatedAtUnixSec: 125,
        };
        this.write({ id: message.id, result: { file: this.artifactFile } });
        return;
      }
      if (message.method === "artifact/file/find") {
        this.write({
          id: message.id,
          result: this.artifactState && this.artifactFile
            ? { entry: { state: this.artifactState, file: this.artifactFile } }
            : { entry: null },
        });
        return;
      }
      if (message.method === "artifact/cache/write") {
        const params = message.params as JsonMessage;
        this.artifactCacheEntry = {
          namespace: String(params.namespace),
          key: String(params.key),
          artifactId: String(params.artifactId),
          status: String(params.status),
          metadata: params.metadata,
          createdAtUnixSec: 126,
          updatedAtUnixSec: 126,
          lastHitAtUnixSec: null,
        };
        this.write({ id: message.id, result: { entry: this.artifactCacheEntry } });
        return;
      }
      if (message.method === "artifact/cache/read") {
        this.write({ id: message.id, result: { entry: this.artifactCacheEntry } });
        return;
      }
      if (message.method === "artifact/cache/delete") {
        this.artifactCacheEntry = null;
        this.write({ id: message.id, result: {} });
        return;
      }
    }
    if (message.method === "apiCatalog/read") {
      this.apiCatalogReadParams = message.params as JsonMessage;
      this.write({
        id: message.id,
        result: {
          schemaVersion: 1,
          generatedAt: 123,
          appServerMethods: [
            {
              method: "thread/start",
              paramsType: "v2::ThreadStartParams",
              responseType: "v2::ThreadStartResponse",
              experimental: false,
              description: null,
            },
            {
              method: "artifact/state/read",
              paramsType: "v2::ArtifactStateReadParams",
              responseType: "v2::ArtifactStateReadResponse",
              experimental: false,
              description: null,
            },
          ],
          mcpServers: [],
          builtInTools: [],
          workflowRuntime: {
            package: "@openai/codex-sdk",
            importSpecifier: "@openai/codex-sdk/workflow",
            symbols: [
              {
                name: "WorkflowContext.artifacts.readState",
                kind: "method",
                signature: "ctx.artifacts.readState(params): Promise<ArtifactStateReadResponse>",
                description: "Read a workflow artifact state by namespace, scope key, and source key.",
              },
            ],
          },
          workflows: [],
        },
      });
      return;
    }
    if (message.method === "workflowRun/start") {
      this.workflowRunParams = message.params as JsonMessage;
      this.write({
        id: message.id,
        result: {
          run: {
            id: "run-1",
            workflowId: String((message.params as JsonMessage).id),
            status: "running",
            threadId: null,
            createdAt: 123,
            startedAt: 123,
            completedAt: null,
            output: null,
            error: null,
          },
        },
      });
      return;
    }
    if (message.method === "workflowRun/wait") {
      this.workflowRunWaitParams = message.params as JsonMessage;
      this.write({
        id: message.id,
        result: {
          completed: true,
          run: {
            id: String((message.params as JsonMessage).runId),
            workflowId: "reports/jira",
            status: "succeeded",
            threadId: null,
            createdAt: 123,
            startedAt: 123,
            completedAt: 124,
            output: { ok: true },
            error: null,
          },
        },
      });
      return;
    }
    if (message.method === "workflowRun/read") {
      this.write({
        id: message.id,
        result: {
          run: {
            id: String((message.params as JsonMessage).runId),
            workflowId: "reports/jira",
            status: "running",
            threadId: null,
            createdAt: 123,
            startedAt: 123,
            completedAt: null,
            output: null,
            error: null,
          },
        },
      });
      return;
    }
    if (message.method === "workflowRun/cancel") {
      this.write({
        id: message.id,
        result: {
          run: {
            id: String((message.params as JsonMessage).runId),
            workflowId: "reports/jira",
            status: "canceled",
            threadId: null,
            createdAt: 123,
            startedAt: 123,
            completedAt: 124,
            output: null,
            error: "workflow run canceled",
          },
        },
      });
      return;
    }
    if (message.method === "workflow/command/execute") {
      this.workflowCommandExecuteParams = message.params as JsonMessage;
      this.write({ id: message.id, result: { message: "listed", data: { ok: true } } });
      return;
    }
    if (typeof message.method === "string" && message.method.startsWith("workflowRun/")) {
      this.workflowNotifications.push(message);
      return;
    }
    if (message.method === "thread/unsubscribe") {
      this.write({ id: message.id, result: {} });
    }
  }

  private write(message: JsonMessage): void {
    this.stdout.write(`${JSON.stringify(message)}\n`);
  }
}

class FakeWebSocket extends EventEmitter {
  static instances: FakeWebSocket[] = [];

  readyState = 0;
  sent: JsonMessage[] = [];
  threadStartParams: JsonMessage | null = null;

  private threadId = "thread-1";

  constructor(
    readonly url: string,
    readonly protocols?: string | string[],
    readonly options?: unknown,
  ) {
    super();
    FakeWebSocket.instances.push(this);
    setImmediate(() => {
      this.readyState = 1;
      this.emit("open", { type: "open" });
    });
  }

  send(data: string): void {
    const message = JSON.parse(data) as JsonMessage;
    this.sent.push(message);
    this.handleMessage(message);
  }

  close(): void {
    if (this.readyState === 3) {
      return;
    }
    this.readyState = 3;
    this.emit("close", { code: 1000, reason: "" });
  }

  addEventListener(event: string, listener: (event: unknown) => void): void {
    this.on(event, listener);
  }

  removeEventListener(event: string, listener: (event: unknown) => void): void {
    this.off(event, listener);
  }

  private handleMessage(message: JsonMessage): void {
    if (message.method === "initialize") {
      this.write({ id: message.id, result: { userAgent: "fake", codexHome: "/tmp/codex" } });
      return;
    }
    if (message.method === "initialized") {
      return;
    }
    if (message.method === "thread/start") {
      this.threadStartParams = message.params as JsonMessage;
      this.write({ id: message.id, result: { thread: { id: this.threadId, turns: [] } } });
      return;
    }
    if (message.method === "thread/unsubscribe") {
      this.write({ id: message.id, result: {} });
    }
  }

  private write(message: JsonMessage): void {
    this.emit("message", { data: JSON.stringify(message) });
  }
}

class FakeUnixAppServer {
  private readonly dir = mkdtempSync(join(tmpdir(), "codex-sdk-unix-"));
  readonly socketPath = join(this.dir, "app-server.sock");
  readonly messages: JsonMessage[] = [];
  private readonly server = createServer((socket) => this.handleSocket(socket));

  async start(): Promise<void> {
    await new Promise<void>((resolve) => this.server.listen(this.socketPath, resolve));
  }

  async close(): Promise<void> {
    await new Promise<void>((resolve) => this.server.close(() => resolve()));
    rmSync(this.dir, { recursive: true, force: true });
  }

  private handleSocket(socket: Socket): void {
    let buffer: Buffer<ArrayBufferLike> = Buffer.alloc(0);
    let handshakeComplete = false;
    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      if (!handshakeComplete) {
        const headerEnd = buffer.indexOf("\r\n\r\n");
        if (headerEnd === -1) {
          return;
        }
        const header = buffer.subarray(0, headerEnd).toString("utf8");
        const key = header
          .split("\r\n")
          .find((line) => line.toLowerCase().startsWith("sec-websocket-key:"))
          ?.split(":")
          .slice(1)
          .join(":")
          .trim();
        if (!key) {
          socket.destroy(new Error("missing websocket key"));
          return;
        }
        const accept = createHash("sha1")
          .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
          .digest("base64");
        socket.write(
          [
            "HTTP/1.1 101 Switching Protocols",
            "Upgrade: websocket",
            "Connection: Upgrade",
            `Sec-WebSocket-Accept: ${accept}`,
            "",
            "",
          ].join("\r\n"),
        );
        handshakeComplete = true;
        buffer = buffer.subarray(headerEnd + 4);
      }
      buffer = this.drainFrames(socket, buffer);
    });
  }

  private drainFrames(
    socket: Socket,
    buffer: Buffer<ArrayBufferLike>,
  ): Buffer<ArrayBufferLike> {
    for (;;) {
      const frame = decodeClientFrame(buffer);
      if (!frame) {
        return buffer;
      }
      buffer = frame.remaining;
      if (frame.opcode !== 0x1) {
        continue;
      }
      const message = JSON.parse(frame.payload.toString("utf8")) as JsonMessage;
      this.messages.push(message);
      this.handleMessage(socket, message);
    }
  }

  private handleMessage(socket: Socket, message: JsonMessage): void {
    if (message.method === "initialize") {
      this.write(socket, {
        id: message.id,
        result: { userAgent: "fake", codexHome: "/tmp/codex" },
      });
      return;
    }
    if (message.method === "initialized") {
      return;
    }
    if (message.method === "thread/start") {
      this.write(socket, {
        id: message.id,
        result: { thread: { id: "thread-unix", turns: [] } },
      });
      return;
    }
    if (message.method === "thread/unsubscribe") {
      this.write(socket, { id: message.id, result: {} });
    }
  }

  private write(socket: Socket, message: JsonMessage): void {
    socket.write(encodeServerFrame(JSON.stringify(message)));
  }
}

function decodeClientFrame(
  buffer: Buffer<ArrayBufferLike>,
): {
  opcode: number;
  payload: Buffer<ArrayBufferLike>;
  remaining: Buffer<ArrayBufferLike>;
} | null {
  if (buffer.length < 2) {
    return null;
  }
  const first = buffer[0]!;
  const second = buffer[1]!;
  let offset = 2;
  let length = second & 0x7f;
  if (length === 126) {
    if (buffer.length < offset + 2) {
      return null;
    }
    length = buffer.readUInt16BE(offset);
    offset += 2;
  } else if (length === 127) {
    if (buffer.length < offset + 8) {
      return null;
    }
    length = Number(buffer.readBigUInt64BE(offset));
    offset += 8;
  }
  const masked = (second & 0x80) !== 0;
  if (!masked || buffer.length < offset + 4 + length) {
    return null;
  }
  const mask = buffer.subarray(offset, offset + 4);
  offset += 4;
  const maskedPayload = buffer.subarray(offset, offset + length);
  const payload = Buffer.from(maskedPayload.map((byte, index) => byte ^ mask[index % 4]!));
  return {
    opcode: first & 0x0f,
    payload,
    remaining: buffer.subarray(offset + length),
  };
}

function encodeServerFrame(message: string): Buffer {
  const payload = Buffer.from(message, "utf8");
  let headerLength = 2;
  if (payload.length >= 126 && payload.length <= 0xffff) {
    headerLength += 2;
  } else if (payload.length > 0xffff) {
    headerLength += 8;
  }
  const frame = Buffer.alloc(headerLength + payload.length);
  frame[0] = 0x81;
  if (payload.length < 126) {
    frame[1] = payload.length;
  } else if (payload.length <= 0xffff) {
    frame[1] = 126;
    frame.writeUInt16BE(payload.length, 2);
  } else {
    frame[1] = 127;
    frame.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  payload.copy(frame, headerLength);
  return frame;
}

function waitForImmediate(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}

describe("CodexWorkflow", () => {
  beforeEach(() => {
    spawnMock.mockReset();
    FakeWebSocket.instances = [];
    delete process.env.CODEX_APP_SERVER_URL;
    delete process.env.CODEX_WORKFLOW_APP_SERVER_URL;
    delete process.env.CODEX_WORKFLOW_APPROVALS;
    delete process.env.CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR;
  });

  it("runs a turn with JavaScript dynamic tools over app-server", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({
      codexPathOverride: "codex",
      baseUrl: "https://example.test",
      config: { model: "gpt-5" },
    });
    const tool = defineTool(
      {
        namespace: "js",
        name: "lookup_weather",
        description: "Looks up weather",
        inputSchema: {
          type: "object",
          properties: { city: { type: "string" } },
          required: ["city"],
          additionalProperties: false,
        },
      },
      (args) => `Weather for ${(args as { city: string }).city}: mild`,
    );

    const agent = await workflow.startAgent({
      tools: [tool],
      approvalPolicy: "never",
      sandboxMode: "workspace-write",
    });
    const result = await agent.run("Use the weather tool");
    const commandArgs = spawnMock.mock.calls[0]?.[1] as string[] | undefined;

    expect(commandArgs).toEqual([
      "--config",
      'model="gpt-5"',
      "--config",
      'openai_base_url="https://example.test"',
      "app-server",
      "--listen",
      "stdio://",
    ]);
    expect(fake.threadStartParams?.dynamicTools).toEqual([
      {
        namespace: "js",
        name: "lookup_weather",
        description: "Looks up weather",
        inputSchema: tool.inputSchema,
        deferLoading: false,
      },
    ]);
    expect(fake.threadStartParams?.approvalPolicy).toBe("never");
    expect(fake.threadStartParams?.sandbox).toBe("workspace-write");
    expect(fake.turnStartParams?.input).toEqual([
      { type: "text", text: "Use the weather tool", text_elements: [] },
    ]);
    expect(fake.toolResponse).toEqual({
      contentItems: [{ type: "inputText", text: "Weather for Paris: mild" }],
      success: true,
    });
    expect(result.finalResponse).toBe("Weather: mild");
    expect(result.status).toBe("completed");
    expect(workflow.listAgents().map((registeredAgent) => registeredAgent.threadId)).toEqual([
      "thread-1",
    ]);

    await agent.close();
    expect(workflow.listAgents()).toEqual([]);

    await workflow.close();
  });

  it("tracks forked agents", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const agent = await workflow.startAgent();
    const forked = await agent.fork();

    expect(workflow.listAgents().map((registeredAgent) => registeredAgent.threadId)).toEqual([
      "thread-1",
      "thread-forked",
    ]);

    await forked.close();
    await agent.close();
    await workflow.close();
  });

  it("resumes persisted agents by thread id", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const agent = await workflow.resumeAgent("thread-existing", {
      approvalPolicy: "never",
      sandboxMode: "read-only",
    });

    expect(fake.threadResumeParams).toEqual({
      threadId: "thread-existing",
      approvalPolicy: "never",
      sandbox: "read-only",
    });
    expect(agent.threadId).toBe("thread-existing");
    expect(workflow.listAgents().map((registeredAgent) => registeredAgent.threadId)).toEqual([
      "thread-existing",
    ]);

    await agent.close();
    await workflow.close();
  });

  it("connects workflows to a shared app-server over websocket", async () => {
    const workflow = await CodexWorkflow.start({
      appServerUrl: "ws://127.0.0.1:8765",
      webSocket: FakeWebSocket as unknown as WebSocketConstructor,
      webSocketProtocols: "codex",
      webSocketOptions: { headers: { authorization: "Bearer test" } },
    });

    const agent = await workflow.startAgent({ workingDirectory: "/repo" });
    const socket = FakeWebSocket.instances[0];

    expect(spawnMock).not.toHaveBeenCalled();
    expect(socket?.url).toBe("ws://127.0.0.1:8765");
    expect(socket?.protocols).toBe("codex");
    expect(socket?.options).toEqual({ headers: { authorization: "Bearer test" } });
    expect(socket?.threadStartParams).toEqual({ cwd: "/repo", dynamicTools: [] });
    expect(agent.threadId).toBe("thread-1");

    await agent.close();
    await workflow.close();
    expect(socket?.readyState).toBe(3);
  });

  it("uses CODEX_APP_SERVER_URL from the environment", async () => {
    process.env.CODEX_APP_SERVER_URL = "ws://127.0.0.1:8765/";

    const workflow = await CodexWorkflow.start({
      webSocket: FakeWebSocket as unknown as WebSocketConstructor,
    });

    expect(spawnMock).not.toHaveBeenCalled();
    expect(FakeWebSocket.instances[0]?.url).toBe("ws://127.0.0.1:8765/");

    await workflow.close();
  });

  const unixIt = process.platform === "win32" ? it.skip : it;

  unixIt("connects workflows to a shared app-server over a unix socket", async () => {
    const fake = new FakeUnixAppServer();
    await fake.start();

    const workflow = await CodexWorkflow.start({
      appServerUrl: `unix://${fake.socketPath}`,
    });
    const agent = await workflow.startAgent({ workingDirectory: "/repo" });

    expect(spawnMock).not.toHaveBeenCalled();
    expect(agent.threadId).toBe("thread-unix");
    expect(fake.messages.map((message) => message.method)).toEqual([
      "initialize",
      "initialized",
      "thread/start",
    ]);

    await agent.close();
    await workflow.close();
    await fake.close();
  });

  it("requires an existing app-server when requested", async () => {
    await expect(CodexWorkflow.connect({ codexPathOverride: "codex" })).rejects.toThrow(
      "No Codex app-server URL is available",
    );
    expect(spawnMock).not.toHaveBeenCalled();
  });

  it("can force a private app-server even when an environment URL is set", async () => {
    process.env.CODEX_APP_SERVER_URL = "ws://127.0.0.1:8765/";
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.spawnServer({ codexPathOverride: "codex" });

    expect(spawnMock).toHaveBeenCalledTimes(1);
    expect(FakeWebSocket.instances).toEqual([]);

    await workflow.close();
  });

  it("can defer interactive requests for another app-server client", async () => {
    const fake = new FakeAppServerProcess();
    fake.sendApprovalRequestOnThreadStart = true;
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({
      codexPathOverride: "codex",
      approvals: "delegate",
    });

    await workflow.startAgent();
    await waitForImmediate();
    await waitForImmediate();

    expect(fake.approvalResponse).toBeNull();

    await workflow.close();
  });

  it("declines interactive requests by default", async () => {
    const fake = new FakeAppServerProcess();
    fake.sendApprovalRequestOnThreadStart = true;
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });

    await workflow.startAgent();
    await waitForImmediate();
    await waitForImmediate();

    expect(fake.approvalResponse).toEqual({
      id: "approval-request-1",
      result: { decision: "decline" },
    });

    await workflow.close();
  });

  it("can answer interactive requests with an approval handler", async () => {
    const fake = new FakeAppServerProcess();
    fake.sendApprovalRequestOnThreadStart = true;
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({
      codexPathOverride: "codex",
      approvals: {
        mode: "handler",
        onApproval(request) {
          expect(request.type).toBe("commandExecution");
          expect(request.method).toBe("item/commandExecution/requestApproval");
          return { decision: "accept" };
        },
      },
    });

    await workflow.startAgent();
    await waitForImmediate();
    await waitForImmediate();

    expect(fake.approvalResponse).toEqual({
      id: "approval-request-1",
      result: { decision: "accept" },
    });

    await workflow.close();
  });

  it("reads the Codex API catalog", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const catalog = await workflow.api.read({ mcpDetail: "toolsAndAuthOnly" });

    expect(fake.apiCatalogReadParams).toEqual({ mcpDetail: "toolsAndAuthOnly" });
    expect(catalog.schemaVersion).toBe(1);
    expect(catalog.appServerMethods.map((method) => method.method)).toEqual(
      expect.arrayContaining(["thread/start", "artifact/state/read"]),
    );
    expect(catalog.appServerMethods.some((method) => method.method === "artifact/state/read")).toBe(
      true,
    );
    expect(
      catalog.workflowRuntime.symbols.some(
        (symbol) => symbol.name === "WorkflowContext.artifacts.readState",
      ),
    ).toBe(true);
    expect(catalog.workflows).toEqual([]);

    await workflow.close();
  });

  it("exposes artifact storage APIs on workflow contexts", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const result = await runWorkflow(
      defineWorkflow<{ key: string }, string>({
        name: "artifact-context",
        async run(context, input) {
          const register = await context.artifacts.registerState({
            namespace: "workflow",
            scopeKey: input.key,
            sourceKey: `${input.key}:state`,
            stateDir: "/tmp/workflow-state",
            sources: [{ path: "report.txt", kind: "report", sha256: "abc123" }],
            metadata: { revision: 1 },
          });
          const read = await context.artifacts.readState({
            namespace: "workflow",
            scopeKey: input.key,
            sourceKey: `${input.key}:state`,
          });
          const hit = await context.artifacts.recordStateHit({
            namespace: "workflow",
            stateDir: "/tmp/workflow-state",
          });
          const index = await context.artifacts.indexFile({
            namespace: "workflow",
            stateDir: "/tmp/workflow-state",
            relativePath: "report.txt",
          });
          const cacheWrite = await context.artifacts.writeCacheEntry({
            namespace: "workflow",
            key: input.key,
            artifactId: `${input.key}:state`,
            status: "fresh",
            metadata: { revision: 1 },
          });
          const cacheRead = await context.artifacts.readCacheEntry({
            namespace: "workflow",
            key: input.key,
          });
          await context.artifacts.deleteCacheEntry({
            namespace: "workflow",
            key: input.key,
          });

          expect(register.state.namespace).toBe("workflow");
          expect(read.state?.sourceKey).toBe(`${input.key}:state`);
          expect(hit).toEqual({});
          expect(index.file.relativePath).toBe("report.txt");
          expect(cacheRead.entry).toEqual(cacheWrite.entry);
          return read.state?.namespace ?? "missing";
        },
      }),
      {
        codexPathOverride: "codex",
        input: { key: "reports/jira" },
      },
    );

    expect(result).toBe("workflow");
    expect(fake.artifactRequests.map((request) => request.method)).toEqual([
      "artifact/state/register",
      "artifact/state/read",
      "artifact/state/hit",
      "artifact/file/index",
      "artifact/cache/write",
      "artifact/cache/read",
      "artifact/cache/delete",
    ]);

    expect(fake.killed).toBe(true);
  });

  it("wraps workflow registry commands", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const result = await workflow.workflows.run("reports/jira", { project: "COD" });
    const commandResult = await workflow.workflows.command.execute(["list"]);

    expect(fake.workflowRunParams).toEqual({ id: "reports/jira", input: { project: "COD" } });
    expect(fake.workflowRunWaitParams).toEqual({ runId: "run-1" });
    expect(result.data).toEqual({ ok: true });
    expect(fake.workflowCommandExecuteParams).toEqual({ args: ["list"] });
    expect(commandResult.message).toBe("listed");

    await workflow.close();
  });

  it("uses CODEX_WORKFLOW_APPROVALS from the environment", async () => {
    process.env.CODEX_WORKFLOW_APPROVALS = "delegate";
    const fake = new FakeAppServerProcess();
    fake.sendApprovalRequestOnThreadStart = true;
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });

    await workflow.startAgent();
    await waitForImmediate();
    await waitForImmediate();

    expect(fake.approvalResponse).toBeNull();

    await workflow.close();
  });

  it("keeps the legacy interactive request behavior environment fallback", async () => {
    process.env.CODEX_WORKFLOW_INTERACTIVE_REQUEST_BEHAVIOR = "defer";
    const fake = new FakeAppServerProcess();
    fake.sendApprovalRequestOnThreadStart = true;
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });

    await workflow.startAgent();
    await waitForImmediate();
    await waitForImmediate();

    expect(fake.approvalResponse).toBeNull();

    await workflow.close();
  });

  it("runs reusable workflows as standalone scripts", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);
    const progress: unknown[] = [];
    const markdownReports: string[] = [];
    const results: unknown[] = [];
    const workflow = defineWorkflow<{ prompt: string }, string>({
      name: "standalone-test",
      async run(context, input) {
        context.progress("starting", { prompt: input.prompt });
        context.reportToUserMarkdown(`# Result\n\n${input.prompt}`);
        const agent = await context.createAgent();
        const turn = await agent.run(input.prompt);
        context.result(turn.finalResponse);
        return turn.finalResponse;
      },
    });

    const result = await runWorkflow(workflow, {
      codexPathOverride: "codex",
      input: { prompt: "Use the weather tool" },
      onProgress: (event) => progress.push(event),
      onReportToUserMarkdown: (markdown) => markdownReports.push(markdown),
      onResult: (value) => results.push(value),
    });

    expect(result).toBe("Weather: mild");
    expect(progress).toEqual([{ message: "starting", data: { prompt: "Use the weather tool" } }]);
    expect(markdownReports).toEqual(["# Result\n\nUse the weather tool"]);
    expect(fake.workflowNotifications).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          method: "workflowRun/progress",
          params: expect.objectContaining({
            message: "starting",
            data: { prompt: "Use the weather tool" },
            runId: expect.any(String),
          }),
        }),
        expect.objectContaining({
          method: "workflowRun/reportToUserMarkdown",
          params: expect.objectContaining({
            markdown: "# Result\n\nUse the weather tool",
            runId: expect.any(String),
          }),
        }),
      ])
    );
    expect(results).toEqual(["Weather: mild"]);
    expect(fake.killed).toBe(true);
  });

  it("runs child workflows in the parent workflow context", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);
    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const progress: unknown[] = [];
    const child = defineWorkflow<{ value: string }, string>({
      name: "child",
      run(context, input) {
        context.progress("child", input);
        return input.value;
      },
    });
    const parent = defineWorkflow<undefined, string>({
      name: "parent",
      run(context) {
        return context.runWorkflow(child, { value: "done" });
      },
    });

    const result = await runWorkflow(parent, {
      workflow,
      input: undefined,
      onProgress: (event) => progress.push(event),
    });

    expect(result).toBe("done");
    expect(progress).toEqual([{ message: "child", data: { value: "done" } }]);
    expect(fake.workflowNotifications).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          method: "workflowRun/progress",
          params: expect.objectContaining({
            message: "child",
            data: { value: "done" },
            runId: expect.any(String),
          }),
        }),
      ])
    );
    expect(fake.killed).toBe(false);

    await workflow.close();
  });

  it("spawns fresh agents by default", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const agent = await workflow.startAgent();
    const spawned = await agent.spawnAgent();

    expect(workflow.listAgents().map((registeredAgent) => registeredAgent.threadId)).toEqual([
      "thread-1",
      "thread-2",
    ]);

    await spawned.close();
    await agent.close();
    await workflow.close();
  });

  it("exposes MCP and command wrappers", async () => {
    const fake = new FakeAppServerProcess();
    spawnMock.mockReturnValue(fake as unknown as child_process.ChildProcess);

    const workflow = await CodexWorkflow.start({ codexPathOverride: "codex" });
    const agent = await workflow.startAgent();

    await expect(
      workflow.mcp.callTool(agent, { server: "memory", tool: "read", arguments: {} }),
    ).resolves.toEqual({ content: [{ type: "text", text: "ok" }] });
    await expect(workflow.tools.exec(["echo", "done"])).resolves.toEqual({
      exitCode: 0,
      stdout: "done",
      stderr: "",
    });

    await workflow.close();
  });
});
