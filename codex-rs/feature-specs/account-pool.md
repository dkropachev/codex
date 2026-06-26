# Account Pool

## Summary

Account pools let a user configure multiple Codex-capable ChatGPT accounts and use them as one
logical account. Codex smooths quota load across eligible pool members while preserving prompt-cache
locality for each conversation or session. Account choice is pinned for each model operation, and
failover happens only at safe retry boundaries when the expected limit wait outweighs the cache cost.

## Behavior

Account-pool behavior is defined by configuration, member selection, cache-aware switching, usage
refresh, request routing, WebSocket identity, CLI maintenance, and status reporting.

### Goals And Non-Goals

Goals:

- Group multiple named ChatGPT accounts into a single logical pool account.
- Route model requests through a configured default pool when account pooling is enabled.
- Support predictable member selection policies:
  - `drain`: use the first available member until it is unavailable or exhausted.
  - `load_balance`: choose healthy members for cold affinities and preserve cache locality for warm
    or hot affinities unless another member is materially healthier.
- Retry eligible usage-limit failures with the next available member before surfacing the error.
- Keep account-pool state inspectable through CLI and app-server account APIs.
- Preserve normal Codex auth behavior when account pooling is disabled.

Non-goals:

- Combining quota across accounts at the provider. A pool is client-side routing, not a server-side
  entitlement merge.
- Pooling API-key, Bedrock, local, or OSS provider accounts.
- Sharing tokens between accounts.
- Automatically creating or logging in pool member accounts.
- Hiding selected-member diagnostics from clients. A logical pool account still exposes which
  physical member was used.

### Terminology

- Pool: named logical account configured in `config.toml`.
- Member: named account stored under `CODEX_HOME/accounts/<account-id>/auth.json`.
- Default pool: pool used by ordinary Codex requests when pooling is enabled.
- Usage bucket: usage class used for member selection and exhaustion tracking. Current buckets are
  `regular` and `spark`.
- Affinity key: cache-locality key derived from the existing `ModelClient::prompt_cache_key()`.
- Assignment: process-local mapping from pool id, usage bucket, and affinity key to the selected
  member and recent cache metadata.
- Operation: one pinned ModelClient action, such as streaming, realtime setup, WebSocket prewarm,
  compaction, or memory summarization.

### Configuration

Account pools are disabled by default. They are enabled only by explicit `[account_pool]`
configuration.

```toml
[account_pool]
enabled = true
default_pool = "codex-pro"

[account_pool.pools.codex-pro]
provider = "openai"
accounts = ["work-pro", "personal-pro"]
policy = "load_balance"
```

Config fields:

- `account_pool.enabled`: required to activate the feature. `false` or absent means Codex uses the
  normal single-account auth path.
- `account_pool.default_pool`: optional when exactly one pool is configured. It must reference an
  existing pool when present.
- `account_pool.pools.<pool-id>.provider`: must be `openai` for the initial feature.
- `account_pool.pools.<pool-id>.accounts`: ordered member account ids. Empty ids, path separators,
  and parent-directory components are invalid. Order is significant for `drain` and tie-breaking.
- `account_pool.pools.<pool-id>.policy`: `drain` or `load_balance`.

Codex fails config loading with an actionable error when enabled pool config has no pools, a missing
default pool reference, a non-OpenAI provider, an empty member list, or an unsafe member id.

### Account Storage

Pool members use the existing named-account storage layout:

```text
CODEX_HOME/
  accounts/
    work-pro/
      auth.json
    personal-pro/
      auth.json
```

Pool members must use managed ChatGPT auth. Missing credentials or non-ChatGPT auth do not
invalidate the whole pool; that member is unavailable and selection continues with the next eligible
member. Member-level errors are surfaced in pool status.

### Selection Semantics

Common rules:

- Normal model requests use the `regular` bucket.
- Models or request paths that consume a distinct Spark limit use the `spark` bucket.
- Selection is keyed by pool id, usage bucket, and affinity key. Different conversations, sessions,
  and buckets can keep separate assignments.
- Selection considers only members that are not exhausted for the request bucket.
- Missing credentials or unsupported credentials make a member unavailable for selection and record a
  member-level status error.
- Selecting a member for a model operation stores or updates the assignment for that selection key.
- Account choice is pinned for the entire ModelClient operation. Codex must not switch accounts
  mid-stream, during realtime setup, during WebSocket prewarm, or after compaction or memory
  summarization starts.
- Cached auth reads are non-mutating: startup checks, provider capability checks, account display,
  rate-limit reads, and other cached status reads must not activate a pool member or switch the
  current assignment.
- Ties preserve configured member order.
- If no member can provide auth, Codex acts unauthenticated for that request and surfaces the same
  class of auth error as the non-pool path.

`drain` selects the first configured member that is available and not exhausted for the request
bucket. Once selected, that member remains assigned while it remains available.

`load_balance` uses fresh usage data and cache heat to choose an available member:

- Cold affinities prefer the member with the healthiest remaining usage.
- Warm affinities keep the assigned member unless it is exhausted, unavailable, or materially worse
  than another member.
- Hot affinities strongly prefer the assigned member and rebalance only when the assigned member is
  exhausted, unavailable, or significantly worse.

Cache heat is derived from the latest known cached-token state for the affinity:

- Cold: cached input tokens are below 1,000 and cached ratio is below 20%.
- Warm: cache is present but not hot.
- Hot: cached input tokens are at least 10,000 or cached ratio is at least 50%.

### Usage Refresh

For `load_balance`, Codex can refresh usage snapshots before auth selection when cached usage is
missing or stale. Freshness is bounded and currently uses a short interval. The refresh is metadata
for choosing or rebalancing an assignment; it must not switch accounts by itself.

Usage refresh should:

- Fetch all pool members in parallel when possible.
- Use each member's own auth token and account id header.
- Query the ChatGPT Codex usage endpoint for the configured backend base URL.
- Compute remaining percentage from the most constrained available window.
- Track separate remaining values for `regular` and `spark` buckets.
- Bound persisted or in-memory status to small scalar values and short error strings.

Usage refresh must not:

- Persist access tokens outside each member's existing auth storage.
- Log bearer tokens, refresh tokens, or raw auth payloads.
- Block selection indefinitely when one member's usage endpoint hangs or fails.
- Switch the current assignment by itself.

Automatic refresh attempts are throttled to once per minute per pool. Failed refresh attempts count
toward this throttle so an unavailable usage endpoint does not cause repeated refreshes on every auth
selection. Manual refresh entry points may refresh all members and report partial failures, but
manual refresh also does not switch an assignment by itself.

### Request Routing

When account pooling is enabled and a default pool exists:

- Model requests use pool auth instead of default single-account auth.
- Request auth and request headers are derived from the same selected member.
- WebSocket prewarm and the normal request attempt each pin one selected member for that operation.
- Unauthorized recovery and token refresh refresh the selected member, not the default account.
- Non-model calls that require the same ChatGPT backend auth use the same auth manager path unless
  they explicitly need the default account.

When account pooling is disabled, no account-pool path affects auth selection, status reporting, or
token refresh.

### Exhaustion And Retry

The selected member may switch on retry only when all of these are true:

- A model request fails before visible assistant output starts.
- The failure is `usage_limit_reached`.
- One or more rate-limit windows in the error are fully exhausted (`used_percent >= 100`).
- The longest estimated wait from all exhausted windows exceeds the cache-cost threshold.
- Another member is available for the same bucket.

Codex evaluates every exhausted window reported by the error, not only named or currently known
window sizes. If `resets_at` is present, Codex uses it to estimate wait time. If it is missing,
Codex uses a bounded estimate from `window_minutes`.

Cache-cost thresholds:

- Cold cache: switch immediately.
- Warm cache: switch when the estimated wait is at least 2 minutes.
- Hot cache: switch when the estimated wait is at least 10 minutes.

The pool must not switch for:

- `usage_not_included`.
- Any usage error after visible or durable assistant output has started.
- Account status reads, rate-limit reads, cached auth/status inspection, or agent/subagent startup.
- Load-balance refresh results by themselves.

Exhaustion state is process-local and may reset across Codex process restarts.

### WebSocket Identity

Responses WebSocket connections are opened with auth headers for the selected account. A live
connection may be reused only while the selected auth identity matches the identity used to open the
connection. If a permitted account-pool failover changes the selected account, the WebSocket session
must drop incremental request state and reconnect with the new account headers.

### CLI Surface

The CLI supports account-pool inspection and maintenance:

- `codex account list`: list default, named, and configured logical pool accounts.
- `codex account limits`: show Codex usage limits for default, named, and pool member accounts.
- `codex account refresh --pool <pool-id>`: refresh token and usage snapshots for all members in a
  pool.
- `codex login --account <account-id>`: allow logging in a named member account.
- `codex logout --account <account-id>`: allow logging out a named member account.

Pool member accounts should not be duplicated in account list output as ordinary named accounts when
they are already represented by a configured pool.

### App-Server And TUI Surface

`account/read` returns a logical pool account when a pool is active:

```json
{
  "account": {
    "type": "chatgptPool",
    "id": "codex-pro",
    "activeAccountId": "work-pro",
    "members": [
      {
        "id": "work-pro",
        "email": "work@example.com",
        "planType": "pro",
        "active": true,
        "unavailableReason": null,
        "regularRemaining": 82,
        "sparkRemaining": 100,
        "lastError": null
      }
    ]
  },
  "requiresOpenaiAuth": true
}
```

Fields:

- `activeAccountId`: selected member id, or `null` before any member is selected.
- `members[].active`: true for the active member.
- `members[].unavailableReason`: current reason this member cannot be selected, when known.
- `members[].regularRemaining` and `members[].sparkRemaining`: latest known remaining usage
  percentages, or `null` when unknown.
- `members[].lastError`: latest member-level refresh or auth error.

`account/updated` notifies auth mode and plan type changes. It does not carry full pool member
state; clients should use `account/read` for detailed pool display.

`account/rateLimits/read` returns the backward-compatible primary rate-limit snapshot and may return
`rateLimitsByLimitId` keyed by metered `limit_id`, for example `codex`. This read must not switch the
active pool member.

`account/rateLimits/updated` carries sparse rolling rate-limit updates. Clients should merge
available values into the latest known rate-limit state or refetch. Nullable metadata in an update
does not clear previously observed metadata.

The TUI presents a pool as one ChatGPT account with pool metadata. It displays the active member when
known and falls back to the first usable member for display only when no active member exists. Member
count, unavailable count, remaining usage, and member errors are diagnostic status data and must not
imply account switching.

### Telemetry, Logging, Security, And Compatibility

Logging should be enough to diagnose routing without exposing credentials:

- Pool id.
- Selected member id.
- Selection policy.
- Request bucket.
- Exhaustion reason.
- Usage refresh success or failure counts.

Do not log access tokens, refresh tokens, raw auth JSON, or full usage endpoint bodies when they may
contain account metadata beyond the bounded fields needed for diagnostics.

Member ids are local config identifiers and must be validated before being used as path components.
A pool must not let config escape `CODEX_HOME/accounts`. Each member's auth remains isolated in its
own account directory, and pool status must not expose tokens or raw account payloads.

Existing single-account users see no behavior change. Existing named-account auth files remain
compatible. Sessions created with account pools resume without requiring historical selected-member
state. If account-pool config is removed, Codex returns to default auth behavior.

## Entry Points

- [codex-rs/config/src/config_toml.rs](../config/src/config_toml.rs)
- [codex-rs/core/src/config/mod.rs](../core/src/config/mod.rs)
- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)
- [codex-rs/login/src/auth/manager.rs](../login/src/auth/manager.rs)
- [codex-rs/core/src/session/turn.rs](../core/src/session/turn.rs)
- [codex-rs/core/src/client.rs](../core/src/client.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/account.rs](../app-server-protocol/src/protocol/v2/account.rs)
- [codex-rs/app-server/src/request_processors/account_processor.rs](../app-server/src/request_processors/account_processor.rs)
- [codex-rs/tui/src/app_server_session.rs](../tui/src/app_server_session.rs)
- [codex-rs/tui/src/app/background_requests.rs](../tui/src/app/background_requests.rs)
- [codex-rs/tui/src/app/event_dispatch.rs](../tui/src/app/event_dispatch.rs)
- [codex-rs/cli/src/account_list.rs](../cli/src/account_list.rs)
- [codex-rs/cli/src/account_refresh.rs](../cli/src/account_refresh.rs)
- [codex-rs/cli/src/account_usage.rs](../cli/src/account_usage.rs)

## Subfeatures

### Configuration And Account Storage

#### Entry Points

- [codex-rs/config/src/config_toml.rs](../config/src/config_toml.rs)
- [codex-rs/core/src/config/mod.rs](../core/src/config/mod.rs)
- [codex-rs/login/src/auth/manager.rs](../login/src/auth/manager.rs)

#### Invariants

- Account pools are opt-in and only apply when enabled config references a valid pool.
- Pool member ids are validated before they are used as account storage path components.
- Member credentials remain isolated in their named account directories.

### Account Selection And Failover

#### Entry Points

- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)
- [codex-rs/core/src/session/turn.rs](../core/src/session/turn.rs)
- [codex-rs/core/src/client.rs](../core/src/client.rs)

#### Invariants

- Mutating model operations select and pin a pool member according to the configured policy.
- Cached account/status reads do not activate or switch pool members.
- `load_balance` stores assignments separately by usage bucket and affinity key.
- Warm and hot affinities keep their assigned member while the cache benefit outweighs the quota
  benefit of switching.
- Allowed generic usage-limit failover retries with another member for the same bucket before
  visible assistant output starts.
- Disallowed usage errors surface without retrying another member.
- WebSocket request state is dropped and reconnected when a permitted failover changes account
  identity.

### Usage Refresh

#### Entry Points

- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)
- [codex-rs/app-server/src/request_processors/account_processor.rs](../app-server/src/request_processors/account_processor.rs)
- [codex-rs/cli/src/account_refresh.rs](../cli/src/account_refresh.rs)

#### Invariants

- Automatic refresh is throttled per pool.
- Refresh metadata can influence selection but does not switch assignments by itself.
- Manual refresh reports partial failures without switching assignments by itself.
- Refresh status is bounded and excludes tokens or raw auth payloads.

### Account Status And CLI

#### Entry Points

- [codex-rs/app-server-protocol/src/protocol/v2/account.rs](../app-server-protocol/src/protocol/v2/account.rs)
- [codex-rs/app-server/src/request_processors/account_processor.rs](../app-server/src/request_processors/account_processor.rs)
- [codex-rs/tui/src/app_server_session.rs](../tui/src/app_server_session.rs)
- [codex-rs/tui/src/app/background_requests.rs](../tui/src/app/background_requests.rs)
- [codex-rs/tui/src/app/event_dispatch.rs](../tui/src/app/event_dispatch.rs)
- [codex-rs/cli/src/account_list.rs](../cli/src/account_list.rs)
- [codex-rs/cli/src/account_usage.rs](../cli/src/account_usage.rs)

#### Invariants

- Pool status includes active member, member availability, remaining usage, and member errors when
  known.
- Rate-limit status reads and updates do not select or switch pool members.
- CLI account commands show configured pools and member diagnostics without duplicating pool members
  as ordinary named accounts.

## Invariants

- Account pools are used only for configured ChatGPT-backed account IDs.
- Pool selection is tracked per pool id, usage bucket, and affinity key.
- Account choice is pinned for each ModelClient operation.
- `load_balance` prefers healthiest remaining quota for cold affinities and preserves warm or hot
  cache locality unless another member is materially healthier.
- Account switching is limited to fully exhausted `usage_limit_reached` windows before visible
  assistant output starts, and only when the estimated wait exceeds cache cost.
- `usage_not_included` surfaces without retrying another member.
- Non-mutating auth/status reads do not activate or switch pool members.
- Usage refresh data can influence selection but does not switch assignments by itself.
- WebSocket sessions reconnect when permitted failover changes the selected account identity.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise mutating model requests selecting a pool member, cache-aware reuse,
generic usage-limit failover, and disallowed retry cases after visible output or unsupported usage
errors.

#### Test cases

- Regular usage-limit failover retries with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_retries_regular_usage_limit_with_next_member
- Spark usage-limit failover retries with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_retries_spark_usage_limit_with_next_member
- Short-window usage limits retry when wait exceeds cache cost: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_retries_short_usage_limit_when_wait_exceeds_cache_cost
- Hot cache prevents short-wait failover: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_keeps_hot_cache_for_short_wait_usage_limit
- WebSocket failover reconnects with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_websocket_failover_reconnects_with_next_member
- Usage-not-included errors do not retry with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_does_not_retry_usage_not_included_with_next_member
- Usage errors after visible output do not retry with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_does_not_retry_usage_error_after_visible_output
- Usage-limit errors without account pool surface original error: codex-rs/core/tests/suite/account_pool__routing.rs:usage_limit_without_account_pool_surfaces_original_error

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise pool status reads, active member reporting, unavailable member
reporting, rate-limit status reads, and non-mutating status refresh behavior.

#### Test cases

- Account read reports active members: codex-rs/app-server/tests/suite/v2/account_pool__app_server_account.rs:get_account_with_chatgpt_pool
- Account read reports unavailable members: codex-rs/app-server/tests/suite/v2/account_pool__app_server_account.rs:get_account_with_chatgpt_pool_reports_unavailable_members
- Rate-limit status reads are non-mutating: codex-rs/app-server/tests/suite/v2/account_pool__app_server_account.rs:get_account_rate_limits_read_does_not_activate_chatgpt_pool_member
- Token usage status reads are non-mutating: codex-rs/app-server/tests/suite/v2/account_pool__app_server_account.rs:get_account_token_usage_read_does_not_activate_chatgpt_pool_member

### cli (main CLI command behavior)

#### Description

CLI coverage should exercise account-pool account listing and account command behavior visible from
the main Codex command surface.

#### Test cases

- Account list displays account-pool members and credential state: codex-rs/cli/tests/account_pool__account.rs:account_list_human_groups_pool_members_and_statuses,account_list_human_marks_invalid_pool_members,account_list_json_includes_pool_metadata_and_memberships
- Account limits displays account-pool member status: codex-rs/cli/tests/account_pool__account.rs:account_limits_groups_pool_members_and_reports_missing_invalid_in_config_order
- Account refresh reports account-pool member outcomes: codex-rs/cli/tests/account_pool__account.rs:account_refresh_pool_reports_all_missing_credentials,account_refresh_pool_reports_partial_success,account_refresh_pool_reports_blocked_member_and_succeeds_when_another_member_refreshes,account_refresh_pool_fails_when_all_members_are_blocked,account_refresh_pool_fails_when_stale_credentials_cannot_refresh,account_refresh_pool_reports_missing_pool

### tui-e2e (full terminal TUI behavior)

#### Description

Live TUI coverage should exercise the embedded app-server to status-card path for configured
account pools.

#### Test cases

- Status command renders account-pool metadata in a live terminal session: codex-rs/tui/tests/suite/account_pool__live.rs:account_pool_status_renders_in_live_tui

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise pool metadata rendering, display fallback when no active member
is set, and sparse rate-limit update merging.

#### Test cases

- Pool status rendering shows active member and unavailable member metadata: codex-rs/tui/src/status/account_pool__status_tests.rs:status_snapshot_shows_chatgpt_pool_active_member,status_snapshot_shows_chatgpt_pool_unavailable_members
- Pool status rendering shows fallback active-member display: codex-rs/tui/src/status/account_pool__status_tests.rs:status_snapshot_shows_chatgpt_pool_without_active_member
- Pool account-read responses map active and fallback member metadata for status display: codex-rs/tui/src/account_pool__app_server_session.rs:account_ui_state_from_response_preserves_chatgpt_pool_details,account_ui_state_from_response_uses_pool_member_metadata_without_active_assignment
- Sparse rate-limit update merging preserves existing status metadata: codex-rs/tui/src/chatwidget/tests/account_pool__status_and_layout.rs:rolling_rate_limit_snapshot_preserves_prior_individual_limit

### login-auth (auth and login behavior)

#### Description

Auth coverage should exercise token refresh, cached auth, per-affinity assignment, and cache-aware
load balancing for account-pool members.

#### Test cases

- Active member token refresh preserves selected pool member semantics: codex-rs/login/tests/suite/account_pool__auth_refresh.rs:refresh_token_uses_active_account_pool_member
- Cached auth reads do not activate or switch pool members: codex-rs/login/tests/suite/account_pool__selection.rs:cached_auth_read_does_not_activate_or_switch_pool_members
- Cold load-balance selection chooses healthiest fresh remaining quota: codex-rs/login/tests/suite/account_pool__selection.rs:cold_load_balance_selection_chooses_healthiest_remaining_quota
- Cold load-balance selection accounts for existing active affinities: codex-rs/login/tests/suite/account_pool__selection.rs:cold_load_balance_selection_penalizes_existing_affinity_assignments
- Cold load-balance selection accounts for unknown usage: codex-rs/login/tests/suite/account_pool__selection.rs:cold_load_balance_selection_penalizes_unknown_usage
- Hot affinity keeps the assigned account: codex-rs/login/tests/suite/account_pool__selection.rs:hot_affinity_keeps_assigned_account
- Warm affinity rebalances when another member is materially healthier: codex-rs/login/tests/suite/account_pool__selection.rs:warm_affinity_rebalances_when_another_member_is_materially_healthier
- Assignments are separate by affinity key and usage bucket: codex-rs/login/tests/suite/account_pool__selection.rs:assignments_are_separate_by_affinity_key_and_usage_bucket
- Compaction treats existing affinity as a cold cache boundary: codex-rs/login/tests/suite/account_pool__selection.rs:compaction_context_treats_existing_affinity_as_cold_boundary

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Account-pool selection is not exposed through Codex-as-MCP-server tool schemas or MCP tool
execution.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Account-pool behavior does not change MCP client transport, resources, OAuth startup, or recovery
behavior.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Account identity selection is owned by login and core auth handling; the Codex API client transports
requests with the auth headers it is given.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Account-pool behavior is covered by the account CLI and agent paths, not by non-interactive exec mode
semantics.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Account-pool behavior does not currently define telemetry, metric, or export contract changes.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Account-pool behavior does not change exec-server process, filesystem, HTTP, relay, or WebSocket
boundaries.

#### Status

Not covered

## Test Generation Notes

Generate tests for these behaviors:

- Initial `drain` selection chooses the first usable member and marks it active.
- Cold `load_balance` selection chooses the highest fresh remaining usage for an affinity.
- Warm and hot `load_balance` selections keep the assigned member while cache cost outweighs quota
  benefit.
- Warm `load_balance` selection rebalances when another member has materially healthier quota.
- Assignments are independent by pool id, usage bucket, and affinity key.
- Cached auth reads return a usable auth without setting `activeAccountId`.
- Each ModelClient operation selects auth once and pins that account for the operation.
- Starting a new agent/subagent does not switch account unless a request hits an allowed failover
  boundary.
- Exhausted 15-minute, 60-minute, 300-minute, and weekly usage-limit windows can retry with the next
  account when wait exceeds cache cost.
- Multiple exhausted usage-limit windows use the longest relevant wait.
- Hot cache prevents short-wait failover.
- `usage_not_included` surfaces to the user and does not retry with the next account.
- Usage errors after visible assistant output do not retry with the next account.
- Failed automatic usage refresh is throttled; repeated auth selections inside one minute do not
  repeatedly call the usage endpoint.
- A permitted WebSocket failover reconnects with the new account identity instead of reusing an old
  account connection.
- TUI/app-server `account/read` for a pool includes `chatgptPool`, `activeAccountId`, members,
  active flags, unavailable reasons, remaining usage, and last error.
- TUI/app-server `account/rateLimits/read` and `account/rateLimits/updated` update status display
  without selecting or switching accounts.
