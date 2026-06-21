# Account Pool

## Summary

Account pool lets a ChatGPT-backed Codex session choose from a configured set of account IDs. It is
for continuity after hard usage exhaustion, not for opportunistic account churn. Switching accounts
can lose prompt-cache/token-cache benefit, so the active account must remain sticky until it reaches
an allowed hard limit.

## Behavior

The account-pool behavior is defined by the configuration, selection, switching, refresh,
WebSocket, and status contracts below.

### Configuration

Account pool is enabled by `[account_pool]` config. A pool contains:

- `default_pool`: optional ID of the pool used by normal auth selection.
- `pools.<pool_id>.provider`: currently expected to be OpenAI/ChatGPT-backed.
- `pools.<pool_id>.policy`: `drain` or `load_balance`.
- `pools.<pool_id>.accounts`: ordered account IDs, each loaded from `$CODEX_HOME/accounts/<id>/auth.json`.

Without enabled account-pool config, Codex uses the normal single-account auth path.

### Default Behavior

- Normal model requests use the `Regular` bucket.
- Models whose name selects Spark routing use the `Spark` bucket.
- `drain` chooses the first configured non-exhausted account.
- `load_balance` may choose the account with the most fresh remaining usage only when there is no
  active account for the requested bucket.
- Once an account is active, both `drain` and `load_balance` keep using it while it is available.
- Cached auth reads are non-mutating: startup checks, provider capability checks, account display,
  and other cached reads must not activate a pool member or switch the active member.
- Missing credentials and non-ChatGPT credentials make a member unavailable for selection and
  should be surfaced in pool status.

### Switching Rules

The active account may switch only when all of these are true:

- A model request fails before visible assistant output starts.
- The failure is `usage_limit_reached`.
- A rate-limit window in the error is fully exhausted (`used_percent >= 100`).
- The exhausted window duration is one of:
  - 300 minutes (5-hour window)
  - 10080 minutes (weekly window)
- Another member is available for the same bucket.

The pool must not switch for:

- Short-window limits such as 15-minute or 60-minute windows.
- `usage_not_included`.
- Any usage error after visible assistant output has started.
- Account status reads, rate-limit reads, startup prewarm, or agent/subagent startup.
- Load-balance refresh results while the current active account is still available.

When a Spark bucket limit exhausts the active member, both Spark and Regular availability for that
member are marked exhausted to avoid immediately reusing the same account on a regular retry path.

### Usage Refresh Behavior

For `load_balance`, Codex can refresh usage snapshots before auth selection when cached usage is
stale. This refresh is metadata for choosing an initial account; it must not displace an active
account that remains available.

Usage refresh attempts are throttled to once per minute per pool. Failed refresh attempts count
toward this throttle so an unavailable usage endpoint does not cause repeated refreshes on every
auth selection.

Manual refresh entry points may refresh all members and report partial failures. Manual refresh
does not by itself switch the active account.

### WebSocket Behavior

Responses WebSocket connections are opened with auth headers for the selected account. A live
connection may be reused only while the selected auth identity matches the identity used to open the
connection. If a permitted account-pool failover changes the selected account, the WebSocket session
must drop incremental state and reconnect with the new account headers.

### TUI And App-Server API

The TUI observes account-pool state through app-server v2 APIs and notifications.

#### `account/read`

Returns the current provider account. For account pools, the account shape is:

```json
{
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
      "regularRemaining": 73,
      "sparkRemaining": 100,
      "lastError": null
    }
  ]
}
```

`activeAccountId` may be null before the first mutating auth selection. Member emails, plans, and
remaining usage values may be null when unavailable.

#### `account/updated`

Notifies auth mode and plan type changes. This notification does not carry full pool member state;
clients should use `account/read` for detailed pool display.

#### `account/rateLimits/read`

Returns:

- `rateLimits`: backward-compatible primary rate-limit snapshot.
- `rateLimitsByLimitId`: optional map keyed by metered `limit_id`, for example `codex`.

The TUI fetches this during startup/status refresh and converts the response into status snapshots.
This read must not switch the active pool member.

#### `account/rateLimits/updated`

Carries sparse rolling rate-limit updates. The TUI should merge available values into the latest
known rate-limit state or refetch. Nullable metadata in an update does not clear previously observed
metadata.

### TUI Default Display Behavior

- A ChatGPT pool appears as a ChatGPT account status with pool metadata.
- The active member is displayed when known.
- If there is no active member, the TUI falls back to the first usable member for display only.
- Member count and unavailable count should be available to status surfaces.
- Rate-limit status uses `account/rateLimits/read` and update notifications; it should not imply
  account switching.

## Entry Points

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

## Subfeatures

### Account Selection And Failover

#### Entry Points

- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)
- [codex-rs/core/src/session/turn.rs](../core/src/session/turn.rs)
- [codex-rs/core/src/client.rs](../core/src/client.rs)

#### Invariants

- Mutating model requests select a pool member according to the configured policy.
- Cached account/status reads do not activate or switch pool members.
- Allowed hard-limit failover retries with another member for the same bucket before visible
  assistant output starts.
- Disallowed usage errors surface without retrying another member.

### Usage Refresh

#### Entry Points

- [codex-rs/login/src/auth/account_pool.rs](../login/src/auth/account_pool.rs)
- [codex-rs/app-server/src/request_processors/account_processor.rs](../app-server/src/request_processors/account_processor.rs)

#### Invariants

- Automatic refresh is throttled per pool.
- Refresh metadata can influence initial selection but does not displace an available active
  account.
- Manual refresh reports partial failures without switching the active account by itself.

### TUI And App-Server Status

#### Entry Points

- [codex-rs/app-server-protocol/src/protocol/v2/account.rs](../app-server-protocol/src/protocol/v2/account.rs)
- [codex-rs/app-server/src/request_processors/account_processor.rs](../app-server/src/request_processors/account_processor.rs)
- [codex-rs/tui/src/app_server_session.rs](../tui/src/app_server_session.rs)
- [codex-rs/tui/src/app/background_requests.rs](../tui/src/app/background_requests.rs)
- [codex-rs/tui/src/app/event_dispatch.rs](../tui/src/app/event_dispatch.rs)

#### Invariants

- Pool status includes active member, member availability, remaining usage, and member errors when
  known.
- Rate-limit status reads and updates do not select or switch pool members.

## Invariants

- Account pools are used only for configured ChatGPT-backed account IDs.
- Pool selection is sticky for a bucket while the active account remains available.
- Account switching is limited to fully exhausted 5-hour or weekly `usage_limit_reached` windows
  before visible assistant output starts.
- Non-mutating auth/status reads do not activate or switch pool members.
- Spark exhaustion also exhausts regular availability for the same account.
- Usage refresh data can influence initial selection but does not displace an available active
  member.
- WebSocket sessions reconnect when permitted failover changes the selected account identity.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise mutating model requests selecting a pool member, sticky reuse,
allowed hard-limit failover, and disallowed retry cases after visible output or unsupported usage
errors.

#### Test cases

- Regular usage-limit failover retries with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_retries_regular_usage_limit_with_next_member
- Spark usage-limit failover retries with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_retries_spark_usage_limit_with_next_member
- Short-window usage limits do not retry with next member: codex-rs/core/tests/suite/account_pool__routing.rs:account_pool_does_not_retry_short_usage_limit_with_next_member
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
- Rate-limit status reads are non-mutating: missing
- Rate-limit update notifications preserve previously observed metadata: missing

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

Full TUI coverage should exercise the live account status surface for active pool member display,
unavailable member counts, and rate-limit refresh behavior without triggering account switching.

#### Test cases

- Live TUI status shows active pool member and unavailable member count: missing
- Live TUI rate-limit refresh does not switch accounts: missing

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise pool metadata rendering, display fallback when no active
member is set, and sparse rate-limit update merging.

#### Test cases

- Pool status rendering shows active member and unavailable member metadata: codex-rs/tui/src/status/account_pool__status_tests.rs:status_snapshot_shows_chatgpt_pool_active_member,status_snapshot_shows_chatgpt_pool_unavailable_members
- Pool status rendering shows fallback active-member display: missing
- Sparse rate-limit update merging preserves existing status metadata: missing

### login-auth (auth and login behavior)

#### Description

Auth coverage should exercise token refresh and cached auth behavior for active account-pool
members.

#### Test cases

- Active member token refresh preserves selected pool member semantics: codex-rs/login/tests/suite/account_pool__auth_refresh.rs:refresh_token_uses_active_account_pool_member
- Cached auth reads do not activate or switch pool members: missing

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

Account-pool behavior is covered by the account CLI and agent paths, not by non-interactive exec
mode semantics.

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
- Initial `load_balance` selection chooses the highest fresh remaining usage when no active member
  exists.
- `load_balance` keeps the active member even when another member has more remaining usage.
- Cached auth reads return a usable auth without setting `activeAccountId`.
- Starting a new agent/subagent does not switch account unless a request hits an allowed hard limit.
- 300-minute exhausted usage-limit error before visible output retries with the next account.
- 10080-minute exhausted usage-limit error before visible output retries with the next account.
- 15-minute or 60-minute exhausted usage-limit errors surface to the user and do not retry with the
  next account.
- `usage_not_included` surfaces to the user and does not retry with the next account.
- Usage errors after visible assistant output do not retry with the next account.
- Failed automatic usage refresh is throttled; repeated auth selections inside one minute do not
  repeatedly call the usage endpoint.
- A permitted WebSocket failover reconnects with the new account identity instead of reusing an old
  account connection.
- TUI/app-server `account/read` for a pool includes `chatgptPool`, `activeAccountId`, members, active
  flags, unavailable reasons, remaining usage, and last error.
- TUI/app-server `account/rateLimits/read` and `account/rateLimits/updated` update status display
  without selecting or switching accounts.
