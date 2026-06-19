# Account Pool

## Metadata

- Status: implemented
- Primary crates: `codex-login`, `codex-core`, `codex-model-provider`, `codex-app-server-protocol`, `codex-app-server`, `codex-tui`, `codex-cli`
- Core implementation:
  - `login/src/auth/account_pool.rs`
  - `login/src/auth/manager.rs`
  - `core/src/session/turn.rs`
  - `core/src/client.rs`
- TUI/app-server integration:
  - `app-server-protocol/src/protocol/v2/account.rs`
  - `app-server/src/request_processors/account_processor.rs`
  - `tui/src/app_server_session.rs`
  - `tui/src/app/background_requests.rs`
  - `tui/src/app/event_dispatch.rs`

## Summary

Account pool lets a ChatGPT-backed Codex session choose from a configured set of account IDs. It is
for continuity after hard usage exhaustion, not for opportunistic account churn. Switching accounts
can lose prompt-cache/token-cache benefit, so the active account must remain sticky until it reaches
an allowed hard limit.

## Configuration

Account pool is enabled by `[account_pool]` config. A pool contains:

- `default_pool`: optional ID of the pool used by normal auth selection.
- `pools.<pool_id>.provider`: currently expected to be OpenAI/ChatGPT-backed.
- `pools.<pool_id>.policy`: `drain` or `load_balance`.
- `pools.<pool_id>.accounts`: ordered account IDs, each loaded from `$CODEX_HOME/accounts/<id>/auth.json`.

Without enabled account-pool config, Codex uses the normal single-account auth path.

## Default Behavior

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

## Switching Rules

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

## Usage Refresh Behavior

For `load_balance`, Codex can refresh usage snapshots before auth selection when cached usage is
stale. This refresh is metadata for choosing an initial account; it must not displace an active
account that remains available.

Usage refresh attempts are throttled to once per minute per pool. Failed refresh attempts count
toward this throttle so an unavailable usage endpoint does not cause repeated refreshes on every
auth selection.

Manual refresh entry points may refresh all members and report partial failures. Manual refresh
does not by itself switch the active account.

## WebSocket Behavior

Responses WebSocket connections are opened with auth headers for the selected account. A live
connection may be reused only while the selected auth identity matches the identity used to open the
connection. If a permitted account-pool failover changes the selected account, the WebSocket session
must drop incremental state and reconnect with the new account headers.

## TUI And App-Server API

The TUI observes account-pool state through app-server v2 APIs and notifications.

### `account/read`

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

### `account/updated`

Notifies auth mode and plan type changes. This notification does not carry full pool member state;
clients should use `account/read` for detailed pool display.

### `account/rateLimits/read`

Returns:

- `rateLimits`: backward-compatible primary rate-limit snapshot.
- `rateLimitsByLimitId`: optional map keyed by metered `limit_id`, for example `codex`.

The TUI fetches this during startup/status refresh and converts the response into status snapshots.
This read must not switch the active pool member.

### `account/rateLimits/updated`

Carries sparse rolling rate-limit updates. The TUI should merge available values into the latest
known rate-limit state or refetch. Nullable metadata in an update does not clear previously observed
metadata.

## TUI Default Display Behavior

- A ChatGPT pool appears as a ChatGPT account status with pool metadata.
- The active member is displayed when known.
- If there is no active member, the TUI falls back to the first usable member for display only.
- Member count and unavailable count should be available to status surfaces.
- Rate-limit status uses `account/rateLimits/read` and update notifications; it should not imply
  account switching.

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
