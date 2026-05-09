# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Connecting to MCP servers

Codex can connect to MCP servers configured in `~/.codex/config.toml`. See the configuration reference for the latest MCP server options:

- https://developers.openai.com/codex/config-reference

Local stdio MCP servers can opt into narrower or broader process reuse with
`process_reuse_scope`. The default is `cwd`, which preserves the current
behavior of reusing only for the same resolved launch directory. Use `none` to
disable broker reuse for a server, `project` or `repo` when the server is safe
to share across subdirectories of the detected project or Git checkout, and
`user` only for service-backed servers that do not read workspace state and have
an explicit absolute `cwd`. HTTP MCP servers only support the default scope.

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
review_issue_types = ["correctness", "reliability", "maintainability"]
max_review_fix_rounds = 2

[implement]
enabled = true
mode = "auto"
max_cycles = 2

[repo_ci.github_repos."openai/codex"]
learning_instruction = "Do not run integration tests while validating /implement."
review_issue_types = ["correctness", "security", "compatibility", "ux-config-cli"]
```

Use `codex repo-ci enable --cwd` to enable it for the current repository, and
`codex repo-ci learn --cwd` to discover CI files, write the generated runner
script under Codex home, prepare the local environment, and validate the fast
local checks. The learner uses AI to inspect the repository, generate candidate
local CI commands, run them, and iteratively repair the plan until the fast
runner validates or the bounded retry budget is exhausted. The learner records
the source files and SHA-256 hashes it used;
`codex repo-ci status --cwd` reports when those files changed and the repository
should be learned again.

Use `/codex`, `/codex <instruction>`, or
`codex repo-ci instruction set --cwd --instruction "<instruction>"` to replace
the repository-specific learner directive and immediately relearn. The
CLI stores the supplied text as one whitespace-normalized `learning_instruction`
blob on the current GitHub-repo scope when possible, or the current repository
directory scope otherwise. The blob should be concise, non-contradictory,
specific to repo-ci learning, and include every detail the learner needs to
apply it. Use this for repo-specific learner preferences, such as skipping
integration tests, using a specific Docker image, or ignoring a misleading
Makefile. Use
`codex repo-ci instruction show --cwd`, `clear --cwd`, or `edit --cwd` to inspect
or change the blob without rerunning a full workflow.

For one interactive session, override the configured behavior with
`codex --repo-ci off|local|remote|local-and-remote` at startup or
`/repo-ci inherit|off|local|remote|local-and-remote` inside the TUI. `inherit`
clears the session override and returns to the configured repo/user scopes.
You can also override targeted review scope with
`codex --repo-ci-issue-types correctness,reliability` or
`/repo-ci issues inherit|none|comma-list`, and override the review round limit
with `codex --repo-ci-review-rounds N` or `/repo-ci rounds inherit|N`.

When repo CI is enabled for a trusted repository, Codex compares the worktree at
the start and end of each regular turn. If the turn changed files, repo CI runs
the learned fast local runner before completing the turn. Failing local checks
are fed back into the same turn for repair until the configured local retry
limit is reached. Progress is emitted as structured repo CI status events rather
than generic warnings.

The review/fix loop is configured separately by the implement surface. Set
`[implement] enabled = true`, `mode = "auto"`, and `max_cycles = N`, use
`codex implement enable --max-cycles=N`, or run
`/implement enable --max-cycles=N` in the TUI to run targeted review/fix cycles
after normal agent edits. Use `mode = "implicit"`,
`codex implement implicit --max-cycles=N`, or
`/implement implicit --max-cycles=N` to make review/fix run only for turns
submitted with `/implement <task>`. Use `codex implement disable` or
`/implement disable` to turn off those review/fix cycles while leaving repo CI
validation available. `/implement inherit` clears thread-local overrides. When
implement is enabled without repo CI checks for the current scope, Codex can run
the review/fix loop by itself and skip local/remote CI execution. Set
`review_issue_types = []` to skip targeted review entirely. The legacy repo-ci
`max_review_fix_rounds` value is still honored as a fallback when implement
settings are absent. Scope
resolution prefers session overrides, then `directories`, `github_repos`,
`github_orgs`, and finally `defaults`. If no scope config sets issue types,
Codex falls back to repo-ci's inferred defaults for the repository, or to
`correctness`, `reliability`, and `maintainability` when inference is
unavailable.

When a failure occurs, Codex asks the model selected by `model_router` for the
repo-ci phase to classify the failure as `related`, `unrelated`,
`whole_suite`, or `unknown`. If no model result is available, Codex uses
deterministic fallback classification and never ignores `unknown` or
`whole_suite` failures.

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

## Model router

`[model_router]` enables adaptive routing for internal Codex model calls.
The router treats the current model config as the implicit incumbent, builds a
candidate pool according to `discovery`, applies hard policy rules, then scores
eligible routes with task-class heuristics, candidate metrics, price estimates,
context limits, and optional score biases.

```toml
[model_router]
enabled = true
discovery = "curated" # curated | manual | from_rules
subscription_pricing = "amortized_scarce"
savings_reference = "implicit_incumbent"

[model_router.lifecycle.defaults]
window = "30d"
cost_budget_usd = 10.0
token_budget = 1000000
min_evaluated = 20
min_confidence = 0.8
min_success_rate = 0.9
shadow_allowed = true
promotion_shadow_sample_rate_limit = 0.05
monitoring_shadow_sample_rate_limit = 0.02
auto_promote = true
auto_demote = true

[[model_router.candidates]]
id = "spark"
model = "gpt-5.3-codex-spark"
service_tier = "flex"
reasoning_effort = "inherit"
account_pool = "spark"
intelligence_score = 0.62
median_latency_ms = 1800
input_price_per_million = 0.25
cached_input_price_per_million = 0.025
output_price_per_million = 1.25

[[model_router.candidates]]
id = "work"
model = "gpt-5.4"
reasoning_effort = "medium"
account = "work-pro"

[[model_router.candidates]]
id = "high-quality"
model = "gpt-5.4"
reasoning_effort = "high"

[[model_router.models.rules]]
id = "review-top-only"
type = "require" # require | exclude
tasks = ["/review$/"]
models = [{ provider = "openai", model = "/^gpt-5\\.5/" }]

[[model_router.models.rules]]
id = "spark-no-review"
type = "exclude"
tasks = ["/review$/"]
models = [{ provider = "openai", model = "/spark/" }]

[[model_router.bias.rules]]
id = "spark-triage-bias"
tasks = ["module.repo_ci.triage"]
models = [{ provider = "openai", model = "/spark/" }]
score_bias = 0.15

[[model_router.lifecycle.rules]]
id = "review-strict"
tasks = ["/review$/"]
min_evaluated = 40
min_confidence = 0.9
min_success_rate = 0.95
```

`discovery = "curated"` uses the incumbent, explicit `[[model_router.candidates]]`,
the active provider's available model catalog, and user-defined registered
providers that expose a compatible `/models` endpoint. Inactive built-in
providers such as Bedrock, Ollama, and LM Studio are not probed. `manual` uses
only the incumbent plus explicit candidates. `from_rules` uses the incumbent
plus candidates inferred from
`model_router.models.rules`, `model_router.bias.rules`, and
`model_router.lifecycle.rules`; exact model selectors create candidates directly,
while regex selectors expand only against discovered provider catalogs.

`tasks`, `except_tasks`, `provider`, and `model` selectors accept exact strings or
Rust regexes written as `/regex/`. Matching `require` rules union the allowed
models, matching `exclude` rules subtract from that set, and matching bias rules
add `score_bias` to still-eligible route scores. If hard rules leave no eligible
route, the router reports a policy error instead of silently falling back.

A candidate may set `model`, `model_provider`, `service_tier`, `reasoning_effort`,
`account_pool`, `account`, optional observed metrics such as
`intelligence_score`, `success_rate`, and `median_latency_ms`, and optional
token prices. When model discovery reports a context window for a candidate,
the router excludes that candidate if the estimated request would not fit in the
model's effective context window. Candidates with unknown context limits remain
eligible.
`reasoning_effort = "inherit"` keeps the reasoning level from the parent or
default config. `account_pool` references an existing
`[account_pool.pools.<name>]`; `account` routes to one account id under
`~/.codex/accounts/<account-id>`. Account routes apply when the router starts a
new internal session, such as spawned agents and memory consolidation agents. If
a router candidate cannot be applied, Codex leaves the original model configuration
unchanged and continues with the default model selection.

The router records actual production cost, router exploration overhead, and
counterfactual cost against the implicit incumbent so it can report gross and
net AI-cost savings. Router overhead includes shadow calls, canary extras,
benchmark probes, self-assessments, judges, and verifiers.

Lifecycle state is stored in SQLite. Promotion records live in
`model_router_lifecycle_promotions`, transition and blocked-promotion history
lives in `model_router_lifecycle_events`, and shadow validation/monitoring
samples live in `model_router_shadow_evaluations`. With lifecycle shadowing
enabled, candidate routes remain shadows until their promotion samples pass the
effective gates; promoted candidates become production routes when they are
still present after failover and hard policy filtering. Monitoring samples can
demote a promoted candidate when they fall below gates and `auto_demote = true`;
demoted records are ignored by production selection. Re-promoting a candidate
updates the promotion cache timestamp; previous promotions remain visible in the
event history.

Inspect lifecycle history with a read-only SQLite connection. If the database is
live in WAL mode, either use `sqlite3 -readonly` against the live path or copy
the `.sqlite`, `.sqlite-wal`, and `.sqlite-shm` files together before querying.

```sql
SELECT datetime(created_at_ms / 1000, 'unixepoch') AS created,
       event_type, source, task_key, candidate_identity,
       previous_status, next_status, reason,
       shadow_phase, shadow_evaluated_count, shadow_success_rate,
       shadow_average_confidence, shadow_latest_evaluation_id,
       failed_gates_json
FROM model_router_lifecycle_events
ORDER BY created_at_ms DESC, id DESC
LIMIT 50;
```

Inside the TUI, `/model-router enable|disable|inherit|status` temporarily overrides
whether the current thread uses the configured model router. This override lasts
until the session ends or you return to `inherit`, and `enable` requires an
existing `[model_router]` configuration.

The CLI exposes maintenance surfaces under `codex model-router`: `policy` shows
effective candidates, hard-rule eligibility, score bias, and lifecycle gates;
`lifecycle --events --window 30d --candidate-identity <key>` shows current
status, promotion/demotion/blocked counts, auto/manual splits, reasons, and the
event timeline; `shadows` lists shadow evaluation summaries and recent samples;
`promote` and `demote` update lifecycle state and append manual events; `tune`
replays candidate shadows and persists promotion samples while `report
show|apply` keeps managing metric overlays.

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
