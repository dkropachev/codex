# Rune Workflow SDK Examples

These examples show the embedded Rune workflow SDK surface exposed through
`ctx`. They are snippets for workflow authors, not standalone Cargo examples.

- `quickstart.rn`: start an agent and collect a `RunResult`.
- `streaming.rn`: consume a turn stream and collect matching notifications.
- `dynamic_tool.rn`: define a Rune dynamic tool and pass it to an agent.
- `approvals.rn`: configure an existing app-server connection and handle approvals.
- `namespaces.rn`: call high-level app-server namespaces without raw RPC.

Raw `ctx.appServer.request(method, params)` remains available for app-server
methods that do not have a dedicated helper yet. Rune workflows should read
persisted Codex state through app-server APIs, not by opening Codex SQLite
databases directly.
