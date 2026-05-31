# Workflow API Cleanup Plan

## Context

The workflow APIs were moved behind app-server and are now async. The wire-level split is good:

- `workflow/*` manages workflow definitions.
- `workflowRun/*` controls live asynchronous workflow executions.
- Agent turns, command execution, MCP calls, artifacts, and dynamic tools also go through app-server-backed APIs.

The client-side SDK should stop exposing raw RPC-shaped calls as the primary workflow authoring experience. App-server can keep low-level RPCs as implementation details where needed, but workflow code should use clear handle-based APIs that encode user intent.

## Runtime Blocker

The app-server-owned workflow runtime currently advertises an async SDK context, but the process runtime injects only legacy helpers. This caused the observed `code-review` run to stall before review start.

Fix the runtime context first:

- Inject `ctx.startAgent`, `ctx.workflowRuns`, `ctx.workflowRegistry`, `ctx.artifacts`, `ctx.mcp`, `ctx.commands`, `ctx.api`, and related app-server-backed helpers into the process workflow runtime.
- Stop requiring workflows to call `CodexWorkflow.start()` from inside an app-server-owned run.
- Add a timeout or fail-fast path for SDK connection initialization so a bad app-server socket cannot hang forever.

## SDK Type Drift

Do not hand-maintain app-server protocol types in the workflow SDK.

Current drift already exists:

- `WorkflowValidationInfo` is typed as `{ status, messages }` in the SDK, but app-server returns `{ status, findings }`.
- `WorkflowSummary` omits `commandOptionHints`.
- `workflow/repair` is typed as generic `WorkflowCommandResponse`, but app-server returns structured `WorkflowRepairResponse`.
- `workflow/publish` and `workflow/discard` exist on app-server but are not exposed in the SDK.
- `stageSessionId` is not consistently threaded through SDK calls.

Plan:

- Generate or import app-server TypeScript schema types into the SDK.
- Layer ergonomic classes on top of those generated protocol types.
- Keep compatibility aliases where practical, but make the new handles the documented path.

## Agent API

`createAgent` sounds like object construction, but the operation starts an app-server-managed thread. Use `startAgent` as the primary API.

Target:

```ts
const agent = await ctx.startAgent({
  model: "gpt-5.5",
  workingDirectory,
});

const result = await agent.run("Review this change");
```

Plan:

- Make `ctx.startAgent(...)` the primary public method.
- Keep `ctx.createAgent(...)` as a deprecated alias.
- Keep `AgentHandle.run`, `runStreamed`, `fork`, `wait`, and `close`.

## Workflow Run API

Registered workflow execution should be strongly typed by importing the workflow client the caller wants to run. Avoid making normal user workflow code pass workflow ids and untyped JSON through `ctx.workflows.run(id, input)`.

Current confusion:

- `ctx.runWorkflow(...)` used to run an in-memory workflow object in the same JS context.
- Runtime/docs also implied `ctx.runWorkflow(...)` could run registered child workflows by id, including one path that shelled out to `codex workflow run`.
- `ctx.workflows.run(...)` starts and waits for a registered workflow through app-server by string id, which loses input/output type information.

Target:

```ts
import { codeReview } from "./generated/workflows";

const output = await codeReview.run(ctx, {
  targetRef: "HEAD",
});

const run = await codeReview.start(ctx, {
  targetRef: "HEAD",
});

await run.wait();
await run.cancel();
```

Plan:

- Generate typed workflow client modules from published workflow contracts.
- Make imported workflow clients the documented path for workflow-to-workflow calls.
- The generated client should expose `run(ctx, input)`, `start(ctx, input)`, and draft-aware variants when applicable.
- `run(...)` returns the workflow's declared output type directly.
- `start(...)` returns `WorkflowRunHandle<Output>`.
- Remove `ctx.runWorkflow(...)`; workflow-to-workflow execution should use typed imported workflow clients over app-server, not CLI shell-outs.
- Keep id-based execution under a lower-level name such as `ctx.workflowRuns.start({ id, input })` for dynamic dispatch and tooling.
- Add `WorkflowRunHandle`:

```ts
class WorkflowRunHandle<Output = unknown> {
  readonly id: string;
  readonly workflowId: string;

  read(): Promise<WorkflowRun>;
  wait(options?: { timeoutMs?: number | null }): Promise<WorkflowRun>;
  cancel(): Promise<WorkflowRun>;
  stop(): Promise<WorkflowRun>; // alias for cancel()
  output(): Promise<Output>;
}
```

- Keep low-level `start/read/wait/cancel` RPC wrappers if needed, but do not make them the main API.

## Workflow Run Terminal Events

Use distinct terminal notifications:

- `workflowRun/succeeded`
- `workflowRun/failed`
- `workflowRun/canceled`

Avoid overloading `workflowRun/completed`. If a generic terminal event must remain for compatibility, document it as legacy and prefer the explicit event names for new clients.

## Repair Progress

`workflow/repair` currently emits `workflowRun/progress` using an ad hoc generated id, but that id is not backed by a readable, waitable, or cancelable `WorkflowRun`.

That is misleading.

Plan options:

1. Make repair a real run:

```ts
const run = await ctx.workflowRegistry.repair("code-review");
await run.wait();
await run.cancel();
```

2. Or keep `workflow/repair` synchronous and emit a separate management-command progress notification, not `workflowRun/progress`.

Preferred: make long-running repair a real handle-based operation.

## Workflow Drafts

`stageSessionId` is a useful implementation concept, but it should not be the normal client-facing API.

Use explicit draft objects:

```ts
const draft = await ctx.workflowRegistry.newDraft({
  description: "Create a workflow that reviews docs changes",
});

await draft.develop();
await draft.validate();
const generated = await draft.client();
await generated.run(ctx, { targetRef: "HEAD" });
await draft.publish();
```

For existing workflows:

```ts
const draft = await ctx.workflowRegistry.editDraft("code-review");

await draft.edit("Improve handling of review output");
await draft.validate();
const codeReview = await draft.client();
await codeReview.run(ctx, { targetRef: "HEAD" });
await draft.publish();
```

Naming decisions:

- Use `newDraft(...)` for a workflow that does not exist yet.
- Use `editDraft(id, ...)` for an existing workflow.
- Avoid `stage(...)`; it is implementation-flavored and sounds like Git staging.
- Avoid repeating `workflow` in method names under `ctx.workflowRegistry`.

The wrapper owns `stageSessionId` internally and passes it to the relevant app-server calls.

## Workflow Management API

Keep workflow definition management separate from workflow execution. This is discovery, editing, validation, repair, and draft lifecycle. It is not the primary execution API for statically known workflows.

```ts
const wf = await ctx.workflowRegistry.get("code-review");

await wf.read();
await wf.impact();
await wf.validate();
await wf.repair();
```

Keep registry-style list/read methods for low-level compatibility, but prefer generated typed imports for normal workflow execution.

Example:

```ts
import { codeReview } from "./generated/workflows";

const wf = await ctx.workflowRegistry.get("code-review");
await wf.validate();

const output = await codeReview.run(ctx, { targetRef: "HEAD" });
```

## Commands API

`ctx.tools.exec(...)` is misleading because these are not AI-agent tools. It runs standalone commands through app-server.

Target:

```ts
const result = await ctx.commands.run(["bun", "test"], { cwd });

const proc = await ctx.commands.spawn(["bash"], { tty: true });
await proc.write("echo hello\n");
await proc.resize({ rows: 40, cols: 120 });
await proc.terminate();
```

Plan:

- Add `ctx.commands.run(...)` for buffered execution.
- Add `ctx.commands.spawn(...)` returning a process handle for streaming/PTY execution.
- Keep `ctx.tools.exec(...)` as a deprecated alias.
- Reserve `tools` terminology for AI-callable dynamic tools.

## Dynamic Agent Tools

These are tools available to Codex agents during a turn.

Current:

```ts
const tool = defineTool(spec, handler);
const agent = await ctx.startAgent({ tools: [tool] });
```

Potential improvement:

```ts
const tool = ctx.agentTools.define(spec, handler);
const agent = await ctx.startAgent({ tools: [tool] });
```

Plan:

- Keep `defineTool(...)` for compatibility.
- Consider `ctx.agentTools.define(...)` to make the relationship to agent tools explicit.
- Do not mix this namespace with command execution.

## MCP API

Current MCP API is thin RPC pass-through:

```ts
await ctx.mcp.listServers();
await ctx.mcp.readResource({ server, uri });
await ctx.mcp.callTool(agent, { server, tool, arguments });
```

Add handles:

```ts
const github = ctx.mcp.server("github");

await github.tool("create_issue").call(agent, args);
await github.resource(uri).read();
```

Keep existing methods for low-level access.

## Artifacts API

The workflow authoring API should expose content-scoped caches instead of storage-shaped artifact
RPCs. Workflow code defines the input scope, Codex hashes the matched files, runs the builder only
when needed, and returns the generated output directory.

```ts
const artifact = await ctx.artifacts.cache.ensure({
  namespace: "workflow-tools",
  key: "code-review/tool-bundle",
  scope: {
    include: ["src/tools/**", "package.json", "bun.lock"],
    exclude: ["node_modules/**", "artifacts/**", "state/**"],
  },
  build: async ({ outputDir, reason, scope }) => {
    await Bun.$`bun build src/tools/index.ts --outdir ${outputDir}`;
    return {
      metadata: { entrypoint: "index.js", reason, inputHash: scope.hash },
    };
  },
});

const entrypoint = artifact.path("index.js");
```

Plan:

- Keep low-level app-server artifact RPCs as private SDK implementation detail.
- Advertise `ctx.artifacts.cache.ensure(...)` as the workflow-facing API.
- Workflow validation rejects direct low-level artifact calls in workflow source.

## API Catalog

Current:

```ts
await ctx.api.read();
```

Better:

```ts
await ctx.catalog.read();
```

Plan:

- Consider renaming `ctx.api` to `ctx.catalog`.
- Keep `ctx.api.read()` as a compatibility alias.

## Migration Order

1. Fix runtime context injection so app-server-owned workflows receive the advertised async SDK context.
2. Add SDK initialization timeouts/fail-fast behavior.
3. Generate/import app-server protocol TS types into the SDK.
4. Generate typed workflow clients from published workflow contracts.
5. Add `WorkflowRunHandle`.
6. Add `startAgent` as primary and deprecate `createAgent`.
7. Add `newDraft` and `editDraft` draft handles.
8. Add command, MCP, artifact, and workflow-definition handles.
9. Fix repair progress semantics.
10. Add explicit workflow run terminal notifications.
11. Update docs and API catalog examples to point at typed workflow imports and the new ergonomic API.
