# Feature Specs

Feature specs are the source of truth for user-facing behavior and expected test coverage. They are
internal inputs for implementation review and AI-assisted test generation, not public product
documentation.

The feature ID is derived from the spec filename. For example, `account-pool.md` defines
`account-pool`. Specs must not contain a separate `Feature ID` metadata field.

Tests link to a feature through their filename prefix when possible. For example, the
repo-relative target `codex-rs/core/tests/suite/account_pool__routing.rs:some_test` maps to
`account-pool.md`. As a fallback for legacy or mixed upstream files, a feature spec may explicitly
list a concrete test target whose filename is not feature-prefixed; that target is counted as
declared coverage for the declaring spec only. Cross-feature test ownership remains out of scope.

Feature specs must not link to test files. Test cases list plain repo-relative test targets so the
verifier can check that the files and test functions exist. Test discovery is derived from
feature-prefixed test filenames and test-place path rules; explicit legacy targets are not
auto-discovered.

The verifier scans known test-place directories for feature-prefixed Rust test files. Discovered
mapped tests must be listed in the owning feature spec, and a test place cannot be marked
`Not covered` when mapped tests exist for that feature and place.

Each spec must also declare `Test Places`. This is a per-feature matrix over every catalog entry
below. When a test place applies to the feature, the entry must include `Description` and
`Test cases`. Each test case is a textual behavior expectation followed by either `missing` or a
repo-relative test target like `codex-rs/core/tests/suite/account_pool__routing.rs:test_name`.
Use `missing` for expected behavior that still needs test coverage; the verifier reports those
entries as coverage backlog.
When a test place should not cover the feature, the entry must include only `Description` and
`Status`, with `Status` set to `Not covered`; the description must explain why that test place does
not apply.

Run `just verify-feature-specs` from the repository root to validate specs against the current
branch and print the generated feature coverage report.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Name

Agent E2E

#### Short Description

agent behavior under core integration tests

#### Description

Place test cases here when feature behavior must be exercised through the core agent loop: model
turns, tool calls, model-visible context, approvals, resume or compaction, and user-visible agent
state transitions.

### app-server-api (app-server API behavior)

#### Name

App-Server API

#### Short Description

app-server API behavior

#### Description

Place test cases here when clients observe or control the feature through app-server requests,
responses, notifications, WebSocket flows, or v2 protocol payloads.

### cli (main CLI command behavior)

#### Name

Main CLI

#### Short Description

main CLI command behavior

#### Description

Place test cases here when the feature changes the top-level codex command surface, command
parsing, command output, or user-visible CLI error behavior.

### tui-e2e (full terminal TUI behavior)

#### Name

TUI E2E

#### Short Description

full terminal TUI behavior

#### Description

Place test cases here when the behavior needs a running terminal UI, keyboard input, popup
completion, screen rendering, or terminal state across an interactive TUI session.

### tui-component (focused TUI component behavior)

#### Name

TUI Component

#### Short Description

focused TUI component behavior

#### Description

Place test cases here when the behavior is local to TUI rendering or state, including component
layout, selection state, popups, status surfaces, and component interactions that do not need a full
terminal session.

### login-auth (auth and login behavior)

#### Name

Login Auth

#### Short Description

auth and login behavior

#### Description

Place test cases here when the feature changes login, logout, token refresh, credential selection,
account storage, cached auth semantics, or auth error handling.

### mcp-server (Codex-as-MCP-server behavior)

#### Name

MCP Server

#### Short Description

Codex-as-MCP-server behavior

#### Description

Place test cases here when external MCP clients invoke Codex as an MCP server, depend on Codex MCP
tool schemas, or consume MCP result and error shapes.

### rmcp-client (MCP client transport and resource behavior)

#### Name

RMCP Client

#### Short Description

MCP client transport and resource behavior

#### Description

Place test cases here when Codex acts as an MCP client and the feature changes server startup,
streamable HTTP, OAuth recovery, resource listing, tool discovery, or process cleanup behavior.

### codex-api (Codex API client and protocol behavior)

#### Name

Codex API

#### Short Description

Codex API client and protocol behavior

#### Description

Place test cases here when the feature changes the lower-level Codex API client, SSE handling,
realtime WebSocket protocol, request construction, or model API integration behavior.

### exec-cli (codex exec CLI behavior)

#### Name

Exec CLI

#### Short Description

codex exec CLI behavior

#### Description

Place test cases here when the feature changes non-interactive codex exec semantics, exec-mode
sandbox or approval handling, process behavior, or exec output and error reporting.

### otel (telemetry and export behavior)

#### Name

Telemetry

#### Short Description

telemetry and export behavior

#### Description

Place test cases here when the feature changes telemetry spans, metrics, event attributes, export
routing, runtime summaries, or OTLP behavior.

### exec-server (exec-server service boundary behavior)

#### Name

Exec Server

#### Short Description

exec-server service boundary behavior

#### Description

Place test cases here when the feature changes exec-server process, filesystem, health, HTTP,
relay, or WebSocket service-boundary behavior.

## Feature Index

- [account-pool](account-pool.md)
- [mcp](mcp.md)
- [plugins](plugins.md)
- [skills](skills.md)
- [workflows](workflows.md)
