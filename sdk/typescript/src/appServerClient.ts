import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import readline from "node:readline";
import { Buffer } from "node:buffer";
import { createHash, randomBytes } from "node:crypto";
import { EventEmitter } from "node:events";
import { createConnection, type Socket } from "node:net";

import type { CodexConfigObject } from "./codexOptions";
import { findCodexPath, serializeConfigOverrides } from "./exec";

const INTERNAL_ORIGINATOR_ENV = "CODEX_INTERNAL_ORIGINATOR_OVERRIDE";
const TYPESCRIPT_SDK_ORIGINATOR = "codex_sdk_ts";

export type JsonRpcId = string | number;

export type JsonRpcError = {
  code: number;
  message: string;
  data?: unknown;
};

type JsonRpcRequestMessage = {
  id: JsonRpcId;
  method: string;
  params?: unknown;
};

type JsonRpcNotificationMessage = {
  method: string;
  params?: unknown;
};

type JsonRpcResponseMessage = {
  id: JsonRpcId;
  result: unknown;
};

type JsonRpcErrorMessage = {
  id: JsonRpcId;
  error: JsonRpcError;
};

export type AppServerNotification = {
  method: string;
  params?: unknown;
};

export type AppServerRequest = {
  id: JsonRpcId;
  method: string;
  params?: unknown;
};

export type ClientInfo = {
  name: string;
  title?: string | null;
  version: string;
};

export type AppServerClientOptions = {
  codexPathOverride?: string | null;
  appServerUrl?: string;
  webSocket?: WebSocketConstructor;
  webSocketProtocols?: string | string[];
  webSocketOptions?: unknown;
  env?: Record<string, string>;
  config?: CodexConfigObject;
  baseUrl?: string;
  apiKey?: string;
  clientInfo?: ClientInfo;
  experimentalApi?: boolean;
};

export type WebSocketConstructor = new (
  url: string,
  protocols?: string | string[],
  options?: unknown,
) => WebSocketLike;

export type WebSocketLike = {
  readyState?: number;
  send(data: string): void;
  close(): void;
  addEventListener?: (event: string, listener: (event: unknown) => void) => void;
  removeEventListener?: (event: string, listener: (event: unknown) => void) => void;
  on?: (event: string, listener: (...args: unknown[]) => void) => void;
  off?: (event: string, listener: (...args: unknown[]) => void) => void;
  once?: (event: string, listener: (...args: unknown[]) => void) => void;
};

type PendingRequest = {
  method: string;
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
};

type AppServerTransport = {
  write(message: string): void;
  close(): Promise<void>;
  isClosed(): boolean;
};

export type ServerRequestHandler = (
  request: AppServerRequest,
  client: AppServerClient,
) => Promise<boolean> | boolean;

export type NotificationHandler = (notification: AppServerNotification) => void;

export class AppServerClient {
  private transport: AppServerTransport;
  private nextId = 1;
  private pending = new Map<JsonRpcId, PendingRequest>();
  private notificationHandlers = new Set<NotificationHandler>();
  private serverRequestHandlers = new Set<ServerRequestHandler>();
  private closed = false;
  private closePromise: Promise<void> | null = null;

  private constructor(transport: AppServerTransport) {
    this.transport = transport;
  }

  static async start(options: AppServerClientOptions = {}): Promise<AppServerClient> {
    const appServerUrl = appServerUrlFromOptions(options);
    if (appServerUrl) {
      return this.connect({ ...options, appServerUrl });
    }

    return this.spawn(options);
  }

  static async spawn(options: AppServerClientOptions = {}): Promise<AppServerClient> {
    const child = spawnAppServer(options);
    const client = new AppServerClient(
      childProcessTransport(
        child,
        (line) => client.handleLine(line),
        (error) => client.handleTransportError(error),
        (detail) => client.handleTransportClosed(detail),
      ),
    );
    await client.initialize(options);
    return client;
  }

  static async connect(options: AppServerClientOptions): Promise<AppServerClient> {
    if (!options.appServerUrl) {
      throw new Error("appServerUrl is required when connecting to an existing Codex app-server");
    }

    if (options.appServerUrl.startsWith("unix://")) {
      let client: AppServerClient;
      const transport = await unixWebSocketTransport(
        options.appServerUrl,
        (line) => client.handleLine(line),
        (error) => client.handleTransportError(error),
        (detail) => client.handleTransportClosed(detail),
      );
      client = new AppServerClient(transport);
      await client.initialize(options);
      return client;
    }

    const webSocket = await openWebSocket(options);
    const client = new AppServerClient(
      webSocketTransport(
        webSocket,
        (line) => client.handleLine(line),
        (error) => client.handleTransportError(error),
        (detail) => client.handleTransportClosed(detail),
      ),
    );
    await client.initialize(options);
    return client;
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    if (this.closed) {
      return Promise.reject(new Error("Codex app-server client is closed"));
    }

    const id = this.nextId++;
    const message: JsonRpcRequestMessage =
      params === undefined ? { id, method } : { id, method, params };
    const promise = new Promise<unknown>((resolve, reject) => {
      this.pending.set(id, { method, resolve, reject });
    });
    this.writeMessage(message);
    return promise as Promise<T>;
  }

  notify(method: string, params?: unknown): void {
    const message: JsonRpcNotificationMessage =
      params === undefined ? { method } : { method, params };
    this.writeMessage(message);
  }

  respond(id: JsonRpcId, result: unknown): void {
    this.writeMessage({ id, result });
  }

  reject(id: JsonRpcId, error: JsonRpcError): void {
    this.writeMessage({ id, error });
  }

  onNotification(handler: NotificationHandler): () => void {
    this.notificationHandlers.add(handler);
    return () => this.notificationHandlers.delete(handler);
  }

  onServerRequest(handler: ServerRequestHandler): () => void {
    this.serverRequestHandlers.add(handler);
    return () => this.serverRequestHandlers.delete(handler);
  }

  async close(): Promise<void> {
    if (this.closePromise) {
      return this.closePromise;
    }
    this.closed = true;
    this.closePromise = this.transport.close();
    await this.closePromise;
  }

  private async initialize(options: AppServerClientOptions): Promise<void> {
    await this.request("initialize", {
      clientInfo: options.clientInfo ?? {
        name: "codex_sdk_ts_workflow",
        title: "Codex TypeScript Workflow SDK",
        version: "0.0.0-dev",
      },
      capabilities: {
        experimentalApi: options.experimentalApi ?? true,
      },
    });
    this.notify("initialized");
  }

  private handleLine(line: string): void {
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch (error) {
      throw new Error(`Failed to parse Codex app-server JSON message: ${line}`, { cause: error });
    }

    if (!isJsonObject(message)) {
      return;
    }

    if ("id" in message && "result" in message) {
      this.handleResponse(message as JsonRpcResponseMessage);
      return;
    }

    if ("id" in message && "error" in message) {
      this.handleError(message as JsonRpcErrorMessage);
      return;
    }

    if ("id" in message && typeof message.method === "string") {
      void this.handleServerRequest(message as AppServerRequest);
      return;
    }

    if (typeof message.method === "string") {
      this.handleNotification(message as AppServerNotification);
    }
  }

  private handleResponse(message: JsonRpcResponseMessage): void {
    const pending = this.pending.get(message.id);
    if (!pending) {
      return;
    }
    this.pending.delete(message.id);
    pending.resolve(message.result);
  }

  private handleError(message: JsonRpcErrorMessage): void {
    const pending = this.pending.get(message.id);
    if (!pending) {
      return;
    }
    this.pending.delete(message.id);
    pending.reject(new Error(`${pending.method} failed: ${message.error.message}`));
  }

  private handleNotification(notification: AppServerNotification): void {
    for (const handler of this.notificationHandlers) {
      handler(notification);
    }
  }

  private async handleServerRequest(request: AppServerRequest): Promise<void> {
    for (const handler of this.serverRequestHandlers) {
      if (await handler(request, this)) {
        return;
      }
    }
    this.reject(request.id, {
      code: -32601,
      message: `No handler registered for app-server request ${request.method}`,
    });
  }

  private writeMessage(
    message:
      | JsonRpcRequestMessage
      | JsonRpcNotificationMessage
      | JsonRpcResponseMessage
      | JsonRpcErrorMessage,
  ): void {
    this.transport.write(JSON.stringify(message));
  }

  private handleTransportClosed(detail: string): void {
    this.closed = true;
    const error = new Error(`Codex app-server connection closed${detail ? `: ${detail}` : ""}`);
    for (const pending of this.pending.values()) {
      pending.reject(error);
    }
    this.pending.clear();
  }

  private handleTransportError(error: Error): void {
    this.closed = true;
    for (const pending of this.pending.values()) {
      pending.reject(error);
    }
    this.pending.clear();
  }
}

async function unixWebSocketTransport(
  appServerUrl: string,
  onLine: (line: string) => void,
  onError: (error: Error) => void,
  onClose: (detail: string) => void,
): Promise<AppServerTransport> {
  const { socket, buffered } = await openUnixWebSocket(appServerUrl);
  const parser = new UnixWebSocketFrameParser(onLine, onClose);
  socket.on("data", (chunk: Buffer) => parser.push(chunk));
  socket.once("error", onError);
  socket.once("close", () => onClose("unix websocket closed"));
  if (buffered.length > 0) {
    parser.push(buffered);
  }

  return {
    write(message: string) {
      socket.write(encodeClientWebSocketFrame(message));
    },
    close() {
      return new Promise<void>((resolve) => {
        if (socket.closed || socket.destroyed) {
          resolve();
          return;
        }
        socket.once("close", () => resolve());
        socket.write(encodeClientWebSocketFrame("", 0x8), () => socket.end());
      });
    },
    isClosed() {
      return socket.closed || socket.destroyed;
    },
  };
}

async function openUnixWebSocket(
  appServerUrl: string,
): Promise<{ socket: Socket; buffered: Buffer }> {
  const socketPath = unixSocketPathFromUrl(appServerUrl);
  const socket = createConnection(socketPath);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });

  const key = randomBytes(16).toString("base64");
  socket.write(
    [
      "GET / HTTP/1.1",
      "Host: localhost",
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Key: ${key}`,
      "Sec-WebSocket-Version: 13",
      "",
      "",
    ].join("\r\n"),
  );

  let buffer = Buffer.alloc(0);
  await new Promise<void>((resolve, reject) => {
    const onData = (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      const headerEnd = buffer.indexOf("\r\n\r\n");
      if (headerEnd === -1) {
        return;
      }
      socket.off("data", onData);
      socket.off("error", onError);
      const header = buffer.subarray(0, headerEnd).toString("utf8");
      verifyUnixWebSocketHandshake(header, key);
      buffer = buffer.subarray(headerEnd + 4);
      resolve();
    };
    const onError = (error: Error) => {
      socket.off("data", onData);
      reject(error);
    };
    socket.on("data", onData);
    socket.once("error", onError);
  });

  return { socket, buffered: buffer };
}

function unixSocketPathFromUrl(appServerUrl: string): string {
  const path = appServerUrl.slice("unix://".length);
  if (!path) {
    throw new Error("unix:// app-server URL must include a socket path");
  }
  return decodeURIComponent(path);
}

function verifyUnixWebSocketHandshake(header: string, key: string): void {
  const lines = header.split("\r\n");
  if (!lines[0]?.includes(" 101 ")) {
    throw new Error(`Codex app-server unix websocket handshake failed: ${lines[0] ?? header}`);
  }
  const headers = new Map<string, string>();
  for (const line of lines.slice(1)) {
    const separator = line.indexOf(":");
    if (separator > 0) {
      headers.set(line.slice(0, separator).trim().toLowerCase(), line.slice(separator + 1).trim());
    }
  }
  const expectedAccept = createHash("sha1")
    .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
    .digest("base64");
  if (headers.get("sec-websocket-accept") !== expectedAccept) {
    throw new Error("Codex app-server unix websocket handshake returned an invalid accept key");
  }
}

class UnixWebSocketFrameParser extends EventEmitter {
  private buffer = Buffer.alloc(0);

  constructor(
    private onLine: (line: string) => void,
    private onClose: (detail: string) => void,
  ) {
    super();
  }

  push(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const frame = this.nextFrame();
      if (!frame) {
        return;
      }
      if (frame.opcode === 0x1) {
        this.onLine(frame.payload.toString("utf8"));
      } else if (frame.opcode === 0x8) {
        this.onClose("unix websocket closed by server");
      }
    }
  }

  private nextFrame(): { opcode: number; payload: Buffer } | null {
    if (this.buffer.length < 2) {
      return null;
    }
    const first = this.buffer[0]!;
    const second = this.buffer[1]!;
    let offset = 2;
    let length = second & 0x7f;
    if (length === 126) {
      if (this.buffer.length < offset + 2) {
        return null;
      }
      length = this.buffer.readUInt16BE(offset);
      offset += 2;
    } else if (length === 127) {
      if (this.buffer.length < offset + 8) {
        return null;
      }
      const bigLength = this.buffer.readBigUInt64BE(offset);
      if (bigLength > BigInt(Number.MAX_SAFE_INTEGER)) {
        throw new Error("Codex app-server websocket frame is too large");
      }
      length = Number(bigLength);
      offset += 8;
    }
    const masked = (second & 0x80) !== 0;
    const maskOffset = offset;
    if (masked) {
      offset += 4;
    }
    if (this.buffer.length < offset + length) {
      return null;
    }
    let payload = this.buffer.subarray(offset, offset + length);
    if (masked) {
      const mask = this.buffer.subarray(maskOffset, maskOffset + 4);
      payload = Buffer.from(payload.map((byte, index) => byte ^ mask[index % 4]!));
    }
    this.buffer = this.buffer.subarray(offset + length);
    return { opcode: first & 0x0f, payload };
  }
}

function encodeClientWebSocketFrame(message: string, opcode = 0x1): Buffer {
  const payload = Buffer.from(message, "utf8");
  const mask = randomBytes(4);
  let headerLength = 2;
  if (payload.length >= 126 && payload.length <= 0xffff) {
    headerLength += 2;
  } else if (payload.length > 0xffff) {
    headerLength += 8;
  }
  const frame = Buffer.alloc(headerLength + 4 + payload.length);
  frame[0] = 0x80 | opcode;
  if (payload.length < 126) {
    frame[1] = 0x80 | payload.length;
  } else if (payload.length <= 0xffff) {
    frame[1] = 0x80 | 126;
    frame.writeUInt16BE(payload.length, 2);
  } else {
    frame[1] = 0x80 | 127;
    frame.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  mask.copy(frame, headerLength);
  for (let index = 0; index < payload.length; index += 1) {
    frame[headerLength + 4 + index] = payload[index]! ^ mask[index % 4]!;
  }
  return frame;
}

function childProcessTransport(
  child: ChildProcessWithoutNullStreams,
  onLine: (line: string) => void,
  onError: (error: Error) => void,
  onClose: (detail: string) => void,
): AppServerTransport {
  const stderrChunks: Buffer[] = [];
  const rl = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });
  rl.on("line", (line) => {
    if (line.trim()) {
      onLine(line);
    }
  });
  child.stderr.on("data", (chunk: Buffer | string) => {
    stderrChunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  });
  child.once("exit", (code, signal) => {
    const detail = signal ? `signal ${signal}` : `code ${code ?? 0}`;
    const stderr = Buffer.concat(stderrChunks).toString("utf8");
    onClose(`exited with ${detail}${stderr ? `: ${stderr}` : ""}`);
  });
  child.once("error", onError);

  return {
    write(message: string) {
      if (!child.stdin.write(`${message}\n`)) {
        // Node will buffer writes for us; app-server requests are small control messages.
      }
    },
    close() {
      return new Promise<void>((resolve) => {
        if (child.exitCode !== null || child.killed) {
          resolve();
          return;
        }
        child.once("exit", () => resolve());
        child.kill();
      });
    },
    isClosed() {
      return child.exitCode !== null || child.killed;
    },
  };
}

async function openWebSocket(options: AppServerClientOptions): Promise<WebSocketLike> {
  const WebSocketImpl = options.webSocket ?? globalWebSocketConstructor();
  if (!WebSocketImpl) {
    throw new Error(
      "No WebSocket implementation is available. Pass webSocket from the `ws` package or run on a runtime with global WebSocket support.",
    );
  }

  const socket = new WebSocketImpl(
    options.appServerUrl!,
    options.webSocketProtocols,
    options.webSocketOptions,
  );
  if (socket.readyState === 1) {
    return socket;
  }

  await new Promise<void>((resolve, reject) => {
    let removeOpenListener: () => void = () => undefined;
    let removeErrorListener: () => void = () => undefined;
    let removeCloseListener: () => void = () => undefined;
    const cleanup = () => {
      removeOpenListener();
      removeErrorListener();
      removeCloseListener();
    };
    const onOpen = () => {
      cleanup();
      resolve();
    };
    const onError = (event: unknown) => {
      cleanup();
      reject(webSocketError(event));
    };
    const onClose = () => {
      cleanup();
      reject(new Error(`Codex app-server websocket closed before initialization`));
    };
    removeOpenListener = addSocketListener(socket, "open", onOpen);
    removeErrorListener = addSocketListener(socket, "error", onError);
    removeCloseListener = addSocketListener(socket, "close", onClose);
  });

  return socket;
}

function webSocketTransport(
  socket: WebSocketLike,
  onLine: (line: string) => void,
  onError: (error: Error) => void,
  onClose: (detail: string) => void,
): AppServerTransport {
  addSocketListener(socket, "message", (event) => {
    onLine(webSocketMessageToString(event));
  });
  addSocketListener(socket, "error", (event) => {
    onError(webSocketError(event));
  });
  addSocketListener(socket, "close", (event) => {
    onClose(webSocketCloseDetail(event));
  });

  return {
    write(message: string) {
      socket.send(message);
    },
    close() {
      return new Promise<void>((resolve) => {
        if (socket.readyState === 3) {
          resolve();
          return;
        }
        const onClose = () => {
          removeCloseListener();
          resolve();
        };
        const removeCloseListener = addSocketListener(socket, "close", onClose);
        socket.close();
      });
    },
    isClosed() {
      return socket.readyState === 3;
    },
  };
}

function globalWebSocketConstructor(): WebSocketConstructor | undefined {
  const candidate = (globalThis as { WebSocket?: WebSocketConstructor }).WebSocket;
  return candidate;
}

function addSocketListener(
  socket: WebSocketLike,
  event: string,
  listener: (...args: unknown[]) => void,
): () => void {
  if (socket.addEventListener) {
    const wrapped = (value: unknown) => listener(value);
    socket.addEventListener(event, wrapped);
    return () => socket.removeEventListener?.(event, wrapped);
  }
  socket.on?.(event, listener);
  return () => socket.off?.(event, listener);
}

function webSocketMessageToString(event: unknown): string {
  const data = isJsonObject(event) && "data" in event ? event.data : event;
  if (typeof data === "string") {
    return data;
  }
  if (Buffer.isBuffer(data)) {
    return data.toString("utf8");
  }
  if (data instanceof ArrayBuffer) {
    return Buffer.from(data).toString("utf8");
  }
  if (ArrayBuffer.isView(data)) {
    return Buffer.from(data.buffer, data.byteOffset, data.byteLength).toString("utf8");
  }
  return String(data);
}

function webSocketError(event: unknown): Error {
  if (event instanceof Error) {
    return event;
  }
  if (isJsonObject(event) && event.error instanceof Error) {
    return event.error;
  }
  if (isJsonObject(event) && typeof event.message === "string") {
    return new Error(event.message);
  }
  return new Error("Codex app-server websocket error");
}

function webSocketCloseDetail(event: unknown): string {
  if (!isJsonObject(event)) {
    return "websocket closed";
  }
  const code = typeof event.code === "number" ? event.code : undefined;
  const reason = typeof event.reason === "string" ? event.reason : undefined;
  if (code && reason) {
    return `websocket closed with code ${code}: ${reason}`;
  }
  if (code) {
    return `websocket closed with code ${code}`;
  }
  return reason ? `websocket closed: ${reason}` : "websocket closed";
}

export function appServerUrlFromOptions(options: AppServerClientOptions): string | undefined {
  const env = options.env ?? process.env;
  return options.appServerUrl ?? env.CODEX_APP_SERVER_URL ?? env.CODEX_WORKFLOW_APP_SERVER_URL;
}

function spawnAppServer(options: AppServerClientOptions): ChildProcessWithoutNullStreams {
  const executablePath = options.codexPathOverride || findCodexPath();
  const commandArgs: string[] = [];

  if (options.config) {
    for (const override of serializeConfigOverrides(options.config)) {
      commandArgs.push("--config", override);
    }
  }

  if (options.baseUrl) {
    commandArgs.push("--config", `openai_base_url=${JSON.stringify(options.baseUrl)}`);
  }

  commandArgs.push("app-server", "--listen", "stdio://");

  const env = buildEnv(options);
  return spawn(executablePath, commandArgs, { env, stdio: "pipe" });
}

function buildEnv(options: AppServerClientOptions): Record<string, string> {
  const env: Record<string, string> = {};
  if (options.env) {
    Object.assign(env, options.env);
  } else {
    for (const [key, value] of Object.entries(process.env)) {
      if (value !== undefined) {
        env[key] = value;
      }
    }
  }
  if (!env[INTERNAL_ORIGINATOR_ENV]) {
    env[INTERNAL_ORIGINATOR_ENV] = TYPESCRIPT_SDK_ORIGINATOR;
  }
  if (options.apiKey) {
    env.CODEX_API_KEY = options.apiKey;
  }
  return env;
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
