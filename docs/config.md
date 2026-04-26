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

## Account Pools

`account_pool` can group multiple bounded Codex subscription accounts into one
logical account. Pool members are loaded from named credential directories under
`~/.codex/accounts/<id>/auth.json`; API-key credentials are not valid pool
members.

```toml
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
policy = "load_balance"
accounts = ["work-pro", "personal-pro"]
```

Supported policies are `drain` and `load_balance`.

## Notify

Codex can run a notification hook when the agent finishes a turn. See the configuration reference for the latest notification settings:

- https://developers.openai.com/codex/config-reference

When Codex knows which client started the turn, the legacy notify JSON payload also includes a top-level `client` field. The TUI reports `codex-tui`, and the app server reports the `clientInfo.name` value from `initialize`.

## Repo CI

Repo CI is an experimental feature flag and command surface for learning a
repository's local and remote validation steps.

```toml
[features]
repo_ci = true

[repo_ci.defaults]
enabled = true
automation = "local-and-remote"
local_test_time_budget_sec = 300
max_local_fix_rounds = 3
max_remote_fix_rounds = 2
models = [
  { inherit = true },
  { model = "gpt-5.3-codex-spark", reasoning_effort = "high" },
]
```

Use `codex repo-ci enable --cwd` to enable it for the current repository, and
`codex repo-ci learn --cwd` to discover CI files, write the generated runner
script under Codex home, prepare the local environment, and validate the fast
local checks. The learner records the source files and SHA-256 hashes it used;
`codex repo-ci status --cwd` reports when those files changed and the repository
should be learned again.

For one interactive session, override the configured behavior with
`codex --repo-ci off|local|remote|local-and-remote` at startup or
`/repo-ci inherit|off|local|remote|local-and-remote` inside the TUI. `inherit`
clears the session override and returns to the configured repo/user scopes.

When repo CI is enabled for a trusted repository, Codex compares the worktree at
the start and end of each regular turn. If the turn changed files, Codex runs
the learned fast local runner before completing the turn. Failing local checks
are fed back into the same turn for repair until the configured local retry
limit is reached. Progress is emitted as structured repo CI status events rather
than generic warnings.

When a failure occurs, Codex asks the configured repo CI model chain to classify
the failure as `related`, `unrelated`, `whole_suite`, or `unknown`. Candidate
models are tried in order; if a candidate fails because of context limits,
availability, or another model-call error, Codex falls back to the next
candidate. If no model result is available, Codex uses deterministic fallback
classification and never ignores `unknown` or `whole_suite` failures.

Remote checks use the GitHub CLI. `codex repo-ci watch-pr --cwd` runs through
the existing `gh` authentication and fails if `gh auth status` is not usable.
Automatic remote checks run after local checks pass when automation includes
`remote`; Codex uses existing `gh` credentials, pushes the current branch when a
PR is linked, watches GitHub checks, ignores clearly unrelated partial failures,
and requests repair for related, unknown, or whole-suite failures until the
configured remote retry limit is reached. Whole-suite failures are never treated
as unrelated.

## JSON Schema

The generated JSON Schema for `config.toml` lives at `codex-rs/core/config.schema.json`.

## SQLite State DB

Codex stores the SQLite-backed state DB under `sqlite_home` (config key) or the
`CODEX_SQLITE_HOME` environment variable. When unset, WorkspaceWrite sandbox
sessions default to a temp directory; other modes default to `CODEX_HOME`.

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
