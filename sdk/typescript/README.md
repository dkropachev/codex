# Codex SDK

Embed the Codex agent in your workflows and apps.

The TypeScript SDK wraps the `codex` CLI from `@openai/codex`. It spawns the CLI and exchanges JSONL events over stdin/stdout.

## Installation

```bash
npm install @openai/codex-sdk
```

Requires Node.js 18+.

## Quickstart

```typescript
import { Codex } from "@openai/codex-sdk";

const codex = new Codex();
const thread = codex.startThread();
const turn = await thread.run("Diagnose the test failure and propose a fix");

console.log(turn.finalResponse);
console.log(turn.items);
```

Call `run()` repeatedly on the same `Thread` instance to continue that conversation.

```typescript
const nextTurn = await thread.run("Implement the fix");
```

## Workflows

Use `defineWorkflow()` when JavaScript code should orchestrate Codex in a way that can run from the TUI, from another
workflow, or as a standalone script. Workflow code receives a `WorkflowContext`; the launcher decides whether to reuse an
existing app-server or start a private one.

```typescript
import { defineTool, defineWorkflow, runWorkflow } from "@openai/codex-sdk/workflow";

const lookupIssue = defineTool(
  {
    namespace: "js",
    name: "lookup_issue",
    description: "Returns issue details from the host application",
    inputSchema: {
      type: "object",
      properties: { id: { type: "string" } },
      required: ["id"],
      additionalProperties: false,
    },
  },
  async (args) => {
    const { id } = args as { id: string };
    return `Issue ${id}: failing checkout test`;
  },
);

const workflow = defineWorkflow<{ issueId: string }, string>({
  name: "fix-issue",
  async run(ctx, input) {
    const agent = await ctx.createAgent({ tools: [lookupIssue] });
    const turn = await agent.run(
      `Use lookup_issue, then propose the smallest fix for issue ${input.issueId}`,
    );
    return turn.finalResponse;
  },
});

const summary = await runWorkflow(workflow, {
  input: { issueId: "C-123" },
  connection: "auto",
});

console.log(summary);
```

`connection: "auto"` connects to `appServerUrl`, `CODEX_APP_SERVER_URL`, or `CODEX_WORKFLOW_APP_SERVER_URL` when one is
available. Otherwise it starts `codex app-server --listen stdio://` and shuts it down when the workflow finishes. Use
`connection: "require-existing"` to fail instead of spawning, or `connection: "spawn"` to always start a private server.

`approvals` controls who answers app-server requests that need a decision. This is separate from an agent's
`approvalPolicy`, which controls when Codex asks. When omitted, standalone workflows decline approval requests unless a host
sets `CODEX_WORKFLOW_APPROVALS=delegate` or you choose `approvals: "delegate"` explicitly.

```typescript
await runWorkflow(workflow, {
  approvals: "decline", // safe default for CI/noninteractive scripts
});

await runWorkflow(workflow, {
  approvals: "delegate", // let another client, usually the TUI, answer
});
```

Inside a workflow, agents are backed by app-server threads. Use `fork()` or `createAgent()` when a workflow needs
independent Codex work, then `wait()` to collect a run that was started with initial input.

```typescript
const reviewAgent = await agent.createAgent({
  input: "Review the proposed patch for regressions",
});

const review = await reviewAgent.wait();
console.log(review?.finalResponse);
```

Resume a persisted Codex session by thread ID when the workflow should continue an existing conversation.

```typescript
const agent = await ctx.resumeAgent(process.env.CODEX_THREAD_ID!);
const turn = await agent.run("Continue from the previous session and summarize next steps");
```

The workflow context also exposes app-server MCP and command helpers.

```typescript
const mcpResult = await ctx.mcp.callTool(agent, {
  server: "docs",
  tool: "search",
  arguments: { query: "checkout failure" },
});

const commandResult = await ctx.tools.exec(["git", "status", "--short"]);
```

Workflows can ask Codex for the same machine-readable API catalog exposed by `codex api` and app-server
`apiCatalog/read`. This is useful when handing available MCP/tool/workflow APIs and discovered workflow metadata to an IDE or another coding agent.

```typescript
const catalog = await ctx.api.read({ mcpDetail: "toolsAndAuthOnly" });
```

The context also exposes the workflow registry and command API used by `codex workflow` and `/workflow`.

```typescript
const { workflows } = await ctx.workflows.registry.list();
const result = await ctx.workflows.run("reports/jira-summary", { project: "COD" });
await ctx.workflows.command.execute(["validate", workflows[0].id]);
```

Workflow summaries include `command` when a workflow exposes `workflow.yaml.command`, or a fallback alias for simple ids
without `/`. That alias is the same name you can type as `/cmd` in the TUI or `codex cmd` on the CLI, and the shared
workflow command parser accepts it in `ctx.workflows.command.execute([...])` as well.

For lower-level control, use `CodexWorkflow.start()`, `CodexWorkflow.connect()`, `CodexWorkflow.spawnServer()`, or
`CodexWorkflow.fromTui()` directly.

### Showing workflow agents in the TUI

To see JavaScript workflow progress and results in the regular Codex TUI, enable workflows in `config.toml` and restart `codex`.

```toml
[features]
workflows = true
```

Then launch the workflow from the TUI:

```bash
/workflow list
/workflow run reports/jira-summary --input '{"project":"COD"}'
```

The TUI starts a loopback app-server automatically and runs the same shared workflow command engine as `codex workflow`.
In that mode reusable workflows can use the same `runWorkflow()` entrypoint:

```typescript
await runWorkflow(workflow);
```

Threads started by the workflow appear in `/agent` and replay progress/results through the same transcript UI as other agents.
The TUI also sets `CODEX_WORKFLOW_APPROVALS=delegate`, so command/file approval and MCP elicitation prompts are left for the
TUI while JavaScript dynamic tools are still answered by the workflow process.

If your Node runtime does not provide a global `WebSocket`, pass one explicitly:

```typescript
import WebSocket from "ws";

const workflow = await CodexWorkflow.start({
  webSocket: WebSocket,
});
```

### Streaming responses

`run()` buffers events until the turn finishes. To react to intermediate progress—tool calls, streaming responses, and file change notifications—use `runStreamed()` instead, which returns an async generator of structured events.

```typescript
const { events } = await thread.runStreamed("Diagnose the test failure and propose a fix");

for await (const event of events) {
  switch (event.type) {
    case "item.completed":
      console.log("item", event.item);
      break;
    case "turn.completed":
      console.log("usage", event.usage);
      break;
  }
}
```

### Structured output

The Codex agent can produce a JSON response that conforms to a specified schema. The schema can be provided for each turn as a plain JSON object.

```typescript
const schema = {
  type: "object",
  properties: {
    summary: { type: "string" },
    status: { type: "string", enum: ["ok", "action_required"] },
  },
  required: ["summary", "status"],
  additionalProperties: false,
} as const;

const turn = await thread.run("Summarize repository status", { outputSchema: schema });
console.log(turn.finalResponse);
```

You can also create a JSON schema from a [Zod schema](https://github.com/colinhacks/zod) using the [`zod-to-json-schema`](https://www.npmjs.com/package/zod-to-json-schema) package and setting the `target` to `"openAi"`.

```typescript
const schema = z.object({
  summary: z.string(),
  status: z.enum(["ok", "action_required"]),
});

const turn = await thread.run("Summarize repository status", {
  outputSchema: zodToJsonSchema(schema, { target: "openAi" }),
});
console.log(turn.finalResponse);
```

### Attaching images

Provide structured input entries when you need to include images alongside text. Text entries are concatenated into the final prompt while image entries are passed to the Codex CLI via `--image`.

```typescript
const turn = await thread.run([
  { type: "text", text: "Describe these screenshots" },
  { type: "local_image", path: "./ui.png" },
  { type: "local_image", path: "./diagram.jpg" },
]);
```

### Resuming an existing thread

Threads are persisted in `~/.codex/sessions`. If you lose the in-memory `Thread` object, reconstruct it with `resumeThread()` and keep going.

```typescript
const savedThreadId = process.env.CODEX_THREAD_ID!;
const thread = codex.resumeThread(savedThreadId);
await thread.run("Implement the fix");
```

### Working directory controls

Codex runs in the current working directory by default. To avoid unrecoverable errors, Codex requires the working directory to be a Git repository. You can skip the Git repository check by passing the `skipGitRepoCheck` option when creating a thread.

```typescript
const thread = codex.startThread({
  workingDirectory: "/path/to/project",
  skipGitRepoCheck: true,
});
```

### Controlling the Codex CLI environment

By default, the Codex CLI inherits the Node.js process environment. Provide the optional `env` parameter when instantiating the
`Codex` client to fully control which variables the CLI receives—useful for sandboxed hosts like Electron apps.

```typescript
const codex = new Codex({
  env: {
    PATH: "/usr/local/bin",
  },
});
```

The SDK still injects its required variables (such as `CODEX_API_KEY`) on top of the environment you provide. If you set
`baseUrl`, the SDK passes it as a `--config openai_base_url=...` override.

### Passing `--config` overrides

Use the `config` option to provide additional Codex CLI configuration overrides. The SDK accepts a JSON object, flattens it
into dotted paths, and serializes values as TOML literals before passing them as repeated `--config key=value` flags.

```typescript
const codex = new Codex({
  config: {
    show_raw_agent_reasoning: true,
    sandbox_workspace_write: { network_access: true },
  },
});
```

Thread options still take precedence for overlapping settings because they are emitted after these global overrides.
