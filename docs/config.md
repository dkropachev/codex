# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

MCP tools default to serialized calls. To mark every tool exposed by one server
as eligible for parallel tool calls, set `supports_parallel_tool_calls` on that
server:

```toml
[mcp_servers.docs]
command = "docs-server"
supports_parallel_tool_calls = true
```

Only enable parallel calls for MCP servers whose tools are safe to run at the
same time. If tools read and write shared state, files, databases, or external
resources, review those read/write race conditions before enabling this setting.

## MCP tool approvals

Codex stores approval defaults and per-tool overrides for custom MCP servers
under `mcp_servers` in `~/.codex/config.toml`. Set
`default_tools_approval_mode` on the server to apply a default to every tool,
and use per-tool `approval_mode` entries for exceptions:

```toml
[mcp_servers.docs]
command = "docs-server"
default_tools_approval_mode = "approve"

[mcp_servers.docs.tools.search]
approval_mode = "prompt"
```

## Apps (Connectors)

Use `$` in the composer to insert a ChatGPT connector; the popover lists accessible
apps. The `/apps` command lists available and installed apps. Connected apps appear first
and are labeled as connected; others are marked as can be installed.

## Notify

Codex can run a notification hook when the agent finishes a turn. See the configuration reference for the latest notification settings:

- https://developers.openai.com/codex/config-reference

When Codex knows which client started the turn, the legacy notify JSON payload also includes a top-level `client` field. The TUI reports `codex-tui`, and the app server reports the `clientInfo.name` value from `initialize`.

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

## SQLite State DB

Codex stores the SQLite-backed state DB under `sqlite_home` (config key) or the
`CODEX_SQLITE_HOME` environment variable. When unset, WorkspaceWrite sandbox
sessions default to a temp directory; other modes default to `CODEX_HOME`.

## ChatGPT account pools

`[account_pool]` lets Codex route ChatGPT requests across named accounts stored
under `~/.codex/accounts/<account-id>`. Set `default_pool` to the pool Codex
should use. Each pool defines the OpenAI `provider`, an ordered `accounts` list,
and a `policy`:

- `drain` uses the first available account until it is unavailable or exhausted,
  then moves to the next account.
- `load_balance` prefers the account with the most fresh remaining usage.

```toml
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro", "personal-pro"]
policy = "drain"
```

## Model policy

`[model_policy]` routes internal Codex model calls by source and prompt size.
Rules are checked in order. Prompt size bounds use UTF-8 bytes, not tokens.

```toml
[model_policy]
enabled = true

[[model_policy.rules]]
source = ["subagent", "module.repo_ci"]
max_prompt_bytes = 20000
model = "gpt-5.3-codex-spark"
reasoning_effort = "inherit"
account_pool = "spark"

[[model_policy.rules]]
source = "subagent.review"
min_prompt_bytes = 20001
model = "gpt-5.4"
reasoning_effort = "medium"
account = "work-pro"
```

Supported source selectors include `subagent`, `agent`,
`subagent.review`, `subagent.thread_spawn`, `subagent.memory_consolidation`,
and `module.repo_ci`. Use `source = "*"` to match every internal source, or use
`source = [...]` / `sources = [...]` to match any of several sources. A route may set `model`,
`model_provider`, `reasoning_effort`, `account_pool`, or `account`.
`reasoning_effort = "inherit"` keeps the reasoning level from the parent or
default config. `account_pool` references an existing
`[account_pool.pools.<name>]`; `account` routes to one account id under
`~/.codex/accounts/<account-id>`. Account routes apply when the policy starts a
new internal session, such as spawned agents and memory consolidation agents. If
a policy route cannot be applied, Codex leaves the original model configuration
unchanged and continues with the default model selection.

## Custom CA Certificates

Codex can trust a custom root CA bundle for outbound HTTPS and secure websocket
connections when enterprise proxies or gateways intercept TLS. This applies to
login flows and to Codex's other external connections, including Codex
components that build reqwest clients or secure websocket clients through the
shared `codex-client` CA-loading path and remote MCP connections that use it.

Set `CODEX_CA_CERTIFICATE` to the path of a PEM file containing one or more
certificate blocks to use a Codex-specific CA bundle. If
`CODEX_CA_CERTIFICATE` is unset, Codex falls back to `SSL_CERT_FILE`. If
neither variable is set, Codex uses the system root certificates.

`CODEX_CA_CERTIFICATE` takes precedence over `SSL_CERT_FILE`. Empty values are
treated as unset.

The PEM file may contain multiple certificates. Codex also tolerates OpenSSL
`TRUSTED CERTIFICATE` labels and ignores well-formed `X509 CRL` sections in the
same bundle. If the file is empty, unreadable, or malformed, the affected Codex
HTTP or secure websocket connection reports a user-facing error that points
back to these environment variables.

## Notices

Codex stores "do not show again" flags for some UI prompts under the `[notice]` table.

## Plan mode defaults

`plan_mode_reasoning_effort` lets you set a Plan-mode-specific default reasoning
effort override. When unset, Plan mode uses the built-in Plan preset default
(currently `medium`). When explicitly set (including `none`), it overrides the
Plan preset. The string value `none` means "no reasoning" (an explicit Plan
override), not "inherit the global default". There is currently no separate
config value for "follow the global default in Plan mode".

## Realtime start instructions

`experimental_realtime_start_instructions` lets you replace the built-in
developer message Codex inserts when realtime becomes active. It only affects
the realtime start message in prompt history and does not change websocket
backend prompt settings or the realtime end/inactive message.

Ctrl+C/Ctrl+D quitting uses a ~1 second double-press hint (`ctrl + c again to quit`).
