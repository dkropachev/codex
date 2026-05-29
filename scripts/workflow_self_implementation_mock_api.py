#!/usr/bin/env python3
import argparse
import json
import sys
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from typing import Any
from urllib.parse import urlparse


HOST = "127.0.0.1"


def usage() -> dict[str, Any]:
    return {
        "input_tokens": 0,
        "input_tokens_details": None,
        "output_tokens": 0,
        "output_tokens_details": None,
        "total_tokens": 0,
    }


def event_response_created(response_id: str) -> dict[str, Any]:
    return {"type": "response.created", "response": {"id": response_id}}


def event_response_completed(response_id: str) -> dict[str, Any]:
    return {"type": "response.completed", "response": {"id": response_id, "usage": usage()}}


def event_function_call(call_id: str, name: str, arguments_json: str) -> dict[str, Any]:
    return {
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": call_id,
            "name": name,
            "arguments": arguments_json,
        },
    }


def event_assistant_message(message_id: str, text: str) -> dict[str, Any]:
    return {
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": message_id,
            "content": [{"type": "output_text", "text": text}],
        },
    }


def workflow_implementation_command() -> str:
    files = workflow_files()
    script = f"""python3 - <<'PY'
from pathlib import Path

files = {files!r}
for name, content in files.items():
    path = Path(name)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
print("implemented todo-sweep workflow")
PY
"""
    return script


def workflow_files() -> dict[str, str]:
    return {
        "src/workflow.ts": r'''/// <reference path="./types.d.ts" />
import { readdir, readFile } from "node:fs/promises";
import { join, relative } from "node:path";
import type { WorkflowContext } from "@openai/codex-sdk/workflow";

export interface WorkflowInput {
  maxItems?: number;
}

export type TodoTag = "TODO" | "FIXME" | "XXX";

export interface TodoItem {
  tag: TodoTag;
  file: string;
  line: number;
  text: string;
}

export interface WorkflowOutput {
  total: number;
  byTag: {
    TODO: number;
    FIXME: number;
    XXX: number;
  };
  items: TodoItem[];
  summaryMarkdown: string;
}

const SKIP_DIRS = new Set([".git", ".codex", "node_modules", "artifacts", "state"]);
const MARKER_RE = /\b(TODO|FIXME|XXX)\b:?\s*(.*)/g;

function validateInput(input: WorkflowInput | undefined): Required<WorkflowInput> {
  const maxItems = input?.maxItems ?? Number.POSITIVE_INFINITY;
  if (!Number.isFinite(maxItems) && maxItems !== Number.POSITIVE_INFINITY) {
    throw new Error("maxItems must be a finite positive number");
  }
  if (maxItems <= 0) {
    throw new Error("maxItems must be greater than zero");
  }
  return { maxItems };
}

async function listFiles(root: string, dir = root): Promise<string[]> {
  const entries = await readdir(dir, { withFileTypes: true });
  const files: string[] = [];
  for (const entry of entries) {
    if (entry.name.startsWith(".") && entry.name !== ".env") {
      if (entry.isDirectory() && SKIP_DIRS.has(entry.name)) {
        continue;
      }
    }
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (!SKIP_DIRS.has(entry.name)) {
        files.push(...await listFiles(root, path));
      }
    } else if (entry.isFile()) {
      files.push(path);
    }
  }
  return files.sort();
}

function collectFromFile(root: string, file: string, contents: string): TodoItem[] {
  const items: TodoItem[] = [];
  const rel = relative(root, file);
  const lines = contents.split(/\r?\n/);
  lines.forEach((lineText, index) => {
    MARKER_RE.lastIndex = 0;
    for (const match of lineText.matchAll(MARKER_RE)) {
      items.push({
        tag: match[1] as TodoTag,
        file: rel,
        line: index + 1,
        text: (match[2] ?? "").trim(),
      });
    }
  });
  return items;
}

function summarize(total: number, byTag: WorkflowOutput["byTag"], items: TodoItem[]): string {
  const lines = [
    `# TODO Sweep`,
    "",
    `Found ${total} marker${total === 1 ? "" : "s"}.`,
    "",
    `- TODO: ${byTag.TODO}`,
    `- FIXME: ${byTag.FIXME}`,
    `- XXX: ${byTag.XXX}`,
  ];
  if (items.length > 0) {
    lines.push("", "## Items");
    for (const item of items) {
      lines.push(`- ${item.tag} ${item.file}:${item.line} ${item.text}`);
    }
  }
  return lines.join("\n");
}

export const WorkflowOutput = {
  toTuiMarkdown(result: WorkflowOutput) {
    return { markdown: result.summaryMarkdown };
  },
};

export default async function todo_sweep(ctx: WorkflowContext, input?: WorkflowInput): Promise<WorkflowOutput> {
  const { maxItems } = validateInput(input);
  const root = ctx.cwd || ctx.currentWorkingDirectory || ctx.repoRoot || process.cwd();
  ctx.progress("Scanning TODO markers", { root, maxItems });

  const byTag = { TODO: 0, FIXME: 0, XXX: 0 };
  const allItems: TodoItem[] = [];
  for (const file of await listFiles(root)) {
    let contents: string;
    try {
      contents = await readFile(file, "utf8");
    } catch {
      continue;
    }
    for (const item of collectFromFile(root, file, contents)) {
      byTag[item.tag] += 1;
      allItems.push(item);
    }
  }

  const items = allItems.slice(0, maxItems);
  return {
    total: allItems.length,
    byTag,
    items,
    summaryMarkdown: summarize(allItems.length, byTag, items),
  };
}

export async function complete() {
  return [{ label: "maxItems", value: "{\"maxItems\":10}" }];
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const inputIndex = process.argv.indexOf("--input");
  const rawInput = inputIndex >= 0 ? process.argv[inputIndex + 1] : "{}";
  const input = JSON.parse(rawInput ?? "{}");
  const output = await todo_sweep({
    progress() {},
    reportToUserMarkdown() {},
    status() {},
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
  } as never, input);
  console.log(JSON.stringify(output, null, 2));
}
''',
        "src/types.d.ts": r'''declare module "node:fs/promises" {
  export function readdir(path: string, options: { withFileTypes: true }): Promise<Array<{
    name: string;
    isDirectory(): boolean;
    isFile(): boolean;
  }>>;
  export function readFile(path: string, encoding: "utf8"): Promise<string>;
}

declare module "node:path" {
  export function join(...paths: string[]): string;
  export function relative(from: string, to: string): string;
}
''',
        "src/tests/workflow.positive.test.ts": r'''// workflow-covers: positive progress finalResult
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import workflow, { WorkflowOutput } from "../workflow.ts";

test("workflow scans repository markers and formats markdown", async () => {
  const fixture = await mkdtemp("/tmp/todo-sweep-positive-");
  await writeFile(join(fixture, "a.ts"), "// TODO: tighten config\n");
  await writeFile(join(fixture, "b.ts"), "// FIXME: preserve edits\n// XXX: document retry\n");

  const events: unknown[] = [];
  const output = await workflow({
    cwd: fixture,
    currentWorkingDirectory: fixture,
    repoRoot: fixture,
    workingDirectory: fixture,
    progress(message: string, data: unknown) {
      events.push(["progress", message, data]);
    },
    reportToUserMarkdown() {},
    status() {},
  } as never, { maxItems: 2 });
  const formatted = WorkflowOutput.toTuiMarkdown(output);

  assert.deepEqual(output.byTag, { TODO: 1, FIXME: 1, XXX: 1 });
  assert.equal(output.total, 3);
  assert.equal(output.items.length, 2);
  assert.match(formatted.markdown, /Found 3 markers/);
  assert.equal(events.length, 1);
  await rm(fixture, { recursive: true, force: true });
});
''',
        "src/tests/workflow.load.test.ts": r'''// workflow-covers: load
import assert from "node:assert/strict";
import test from "node:test";
import workflow, { WorkflowOutput } from "../workflow.ts";

test("workflow module loads expected exports", () => {
  assert.equal(typeof workflow, "function");
  assert.equal(typeof WorkflowOutput.toTuiMarkdown, "function");
});
''',
        "src/tests/workflow.autocomplete.test.ts": r'''// workflow-covers: autocomplete
import assert from "node:assert/strict";
import test from "node:test";
import { complete } from "../workflow.ts";

test("workflow exposes maxItems autocomplete", async () => {
  const suggestions = await complete();

  assert.deepEqual(suggestions, [{ label: "maxItems", value: "{\"maxItems\":10}" }]);
});
''',
        "src/tests/workflow.negative.test.ts": r'''// workflow-covers: negative failureUx
import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../workflow.ts";

test("workflow rejects invalid maxItems", async () => {
  await assert.rejects(
    workflow({
      cwd: process.cwd(),
      currentWorkingDirectory: process.cwd(),
      repoRoot: process.cwd(),
      workingDirectory: process.cwd(),
      progress() {},
      reportToUserMarkdown() {},
      status() {},
    } as never, { maxItems: 0 }),
    /maxItems must be greater than zero/
  );
});
''',
    }


class MockHandler(BaseHTTPRequestHandler):
    server_version = "WorkflowSelfImplementationMock/1.0"

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path != "/v1/models":
            self.send_text(404, "not found")
            return
        self.send_json(200, {"object": "list", "data": [{"id": "gpt-5.2", "object": "model"}]})

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        if path != "/v1/responses":
            self.send_text(404, "not found")
            return

        length = int(self.headers.get("content-length") or "0")
        body = self.rfile.read(length) if length else b""
        self.server.request_count += 1
        request_number = self.server.request_count
        sys.stderr.write(f"[mock-api] request {request_number}: {body.decode('utf-8', 'replace')}\n")
        sys.stderr.flush()

        if request_number == 1:
            command_args = {
                "command": workflow_implementation_command(),
                "workdir": ".",
                "timeout_ms": 120000,
            }
            events = [
                event_response_created("resp-1"),
                event_function_call("call-implement", "shell_command", json.dumps(command_args)),
                event_response_completed("resp-1"),
            ]
        elif request_number == 2:
            events = [
                event_response_created("resp-2"),
                event_assistant_message("msg-1", "Implemented todo-sweep workflow."),
                event_response_completed("resp-2"),
            ]
        else:
            self.send_text(500, f"unexpected Responses API request {request_number}")
            return

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.end_headers()
        for event in events:
            payload = json.dumps(event, separators=(",", ":"))
            self.wfile.write(f"event: {event['type']}\ndata: {payload}\n\n".encode())
            self.wfile.flush()

    def log_message(self, format: str, *args: Any) -> None:
        return

    def send_json(self, status: int, payload: Any) -> None:
        body = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def send_text(self, status: int, text: str) -> None:
        body = text.encode()
        self.send_response(status)
        self.send_header("content-type", "text/plain")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class MockServer(ThreadingHTTPServer):
    request_count: int


def main() -> int:
    parser = argparse.ArgumentParser(description="Mock Responses API for workflow self-implementation e2e.")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--port-file", required=True)
    args = parser.parse_args()

    server = MockServer((HOST, args.port), MockHandler)
    server.request_count = 0
    host, port = server.server_address
    with open(args.port_file, "w", encoding="utf-8") as handle:
        handle.write(f"http://{host}:{port}")
    sys.stderr.write(f"[mock-api] listening on http://{host}:{port}\n")
    sys.stderr.flush()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        return 0
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
