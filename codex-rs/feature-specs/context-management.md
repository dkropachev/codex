# Context Management

## Summary

Context management keeps long-running threads usable without allowing one client to replace another client's active work. This stage defines atomic app-server contracts for standalone compaction and idle-only turn starts.

## Behavior

`thread/compact/start` starts only after Core atomically reserves an idle thread. It returns Core's exact `turnId`; a busy thread returns an invalid-request error without interruption. Its optional `source` defaults to `manual`; `automaticContextManagement` selects the automatic trigger for hooks and metrics, while model-backed strategies also carry it in request metadata and compaction analytics.

`turn/start` retains start-or-steer by default. With `startIfIdle: true`, it starts only on an idle thread and otherwise rejects without recording, queueing, or steering the input. Neither RPC retries a rejected or transport-uncertain submission.

## Entry Points

- [codex-rs/protocol/src/turn_input.rs](../protocol/src/turn_input.rs)
- [codex-rs/core/src/codex_thread.rs](../core/src/codex_thread.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/thread.rs](../app-server-protocol/src/protocol/v2/thread.rs)
- [codex-rs/app-server/src/request_processors/thread_processor.rs](../app-server/src/request_processors/thread_processor.rs)

## Subfeatures

None.

## Invariants

- Admission is decided inside Core, not from a stale app-server status snapshot, and a losing request cannot abort or steer the winning turn.
- Returned IDs match lifecycle events, omitted provenance is manual, and every strategy exposes automatic provenance through its supported observability surfaces.
- App-server and exec-server may run on different supported operating systems.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Core coverage proves atomic reservation, exact IDs, automatic provenance, and non-destructive busy rejection.

#### Test cases

- Concurrent idle compactions admit exactly one turn, preserve its exact ID, and reject replacement: codex-rs/core/tests/suite/compact.rs:idle_only_compaction_is_atomic_and_reports_its_exact_turn_id
- Remote and remote-v2 standalone compaction preserve automatic trigger/reason metadata: codex-rs/core/tests/suite/compact_remote.rs:standalone_automatic_compaction_preserves_remote_provenance
- TokenBudget standalone automatic compaction selects auto hooks: codex-rs/core/tests/suite/token_budget.rs:token_budget_automatic_standalone_compaction_runs_auto_hooks
- TokenBudget standalone automatic compaction preserves its correlated lifecycle on auto-selected execution environments: codex-rs/core/tests/suite/token_budget.rs:token_budget_automatic_standalone_compaction_supports_remote_executor

### app-server-api (app-server API behavior)

#### Description

App-server coverage exercises defaults, response correlation, and idle-versus-busy behavior through the public v2 boundary.

#### Test cases

- Omitted-source compaction returns the exact lifecycle ID with manual provenance: codex-rs/app-server/tests/suite/v2/compaction.rs:thread_compact_start_triggers_compaction_and_returns_exact_turn_id
- Automatic-source compaction preserves provenance and exact IDs with auto-selected execution environments: codex-rs/app-server/tests/suite/v2/compaction.rs:thread_compact_start_automatic_source_uses_auto_env_and_exact_turn_id
- Busy compaction rejects without interrupting the active turn: codex-rs/app-server/tests/suite/v2/compaction.rs:thread_compact_start_rejects_busy_thread_without_interrupting_active_turn
- Idle-only turn start accepts idle work and rejects a busy request: codex-rs/app-server/tests/suite/v2/turn_start.rs:turn_start_if_idle_starts_idle_turn_and_rejects_busy_thread
- Omission preserves start-or-steer behavior: codex-rs/app-server/tests/suite/v2/turn_start.rs:turn_start_defaults_to_start_or_steer_while_active

### cli (main CLI command behavior)

#### Description

No top-level CLI surface changes.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

This stage adds no terminal interaction.

#### Status

Not covered

### tui-component (focused TUI component behavior)

#### Description

This stage adds no TUI component behavior.

#### Status

Not covered

### login-auth (auth and login behavior)

#### Description

Authentication behavior is unchanged.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

The MCP-server surface does not expose these RPCs.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

MCP-client transport is unchanged.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

No lower-level API surface changes.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Non-interactive exec behavior is unchanged.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Existing bounded compaction telemetry is reused without changing exporters.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Remote execution remains supported without changing the exec-server boundary.

#### Status

Not covered

## Test Generation Notes

Race two starts, correlate all notifications by returned ID, cover omitted/null/automatic sources, exercise each compaction strategy, and prove transport uncertainty never causes a resend.
