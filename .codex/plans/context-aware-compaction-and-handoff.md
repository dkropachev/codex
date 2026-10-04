# Context-Aware Compaction and Handoff

## Status

- [x] PR 1 implementation: atomic compaction and turn-start contracts.
- [x] PR 1 committed and published in draft PR [#245](https://github.com/dkropachev/codex/pull/245).
- [x] PR 1 fresh Standards, Spec, breaking-change, context, testing, and change-size reviews reached zero findings.
- [ ] PR 1 required CI green and merge. Current blockers are the pre-existing feature-spec verifier failure on `origin/main` and unrelated Bazel timeout flakes after the retry budget was exhausted.
- [ ] PR 2: manual handoff default and `--ask`.
- [ ] PR 3: deferred handoff and recovery.
- [ ] PR 4: context-pressure guidance.
- [ ] PR 5: automatic context management.

## Summary

Rechecking found that most review findings are real:

- Confirmed bugs: stale pre-steer Plans can transfer; bare `/handoff` loses remote-only attachments; leading `-x` is misparsed.
- Confirmed standards issues: oversized central modules, missing snapshots, avoidable test-only APIs, one-use helper, and an oversized single change.
- Partial findings: duplicated gates are a maintainability smell rather than a proven bug; the 8 KiB rule and hint rearming are internally contradictory.
- Resolution: remove the handoff-specific 8 KiB rule and redesign automatic context handling around a model-selected `compact | handoff` decision.
- Retain Plan-style ownership: `ChatWidget` owns turn/UI state; `App` owns cross-thread lifecycle. Do not introduce `HandoffController`.

Deliver this as serial, independently functional PRs. Merge each before starting the next. No hidden activation stage.

## Architecture and Behavior

### Manual handoff

- `/handoff` always means fresh-thread handoff; automatic assessment never overrides explicit `/handoff` or `/compact`.
- Move command preparation, phase-aware local gates, app events, and lifecycle restoration into feature-local handoff modules. Central dispatch/routing files retain only short delegation hooks.
- Centralize genuinely shared blockers behind typed gate enums while preserving stage-specific and App-versus-ChatWidget checks.
- Require a Plan item to be both owned by the exact planning turn and still authoritative after the latest accepted user input. A same-turn steer invalidates earlier Plans in live and replayed flows.
- Route live and queued bare `/handoff` through the attachment-aware prepared-command path.
- Reject leading hyphen options such as `-x`; preserve lone `-` as guidance and require `-- -x` for literal option-like guidance.
- Remove all 8 KiB prompts, constants, copy, and boundary tests. Validate the generated handoff fragment against the repository's existing 10,000-token model-context-item ceiling using the existing token estimator. This is inherited repository safety policy, not a new handoff product limit.

### Context guidance

- The fixed 70% hint recommends both `/compact` and `/handoff`.
- Rearm only after a new `ContextCompaction` item drops adjusted usage below 70%, or after a fresh thread starts.
- Persist the last observed compaction item ID in passive per-thread state so replay cannot rearm from an already-seen compaction; a newly observed background compaction can.
- Rename the opt-in setting to `[tui].auto_context_management_threshold_percent`; retain the inclusive `71..=85` range and disabled-by-default behavior.

### Automatic assessment

Use the existing visible automatic wrap-up turn with a strict final-output schema. Suppress raw selector JSON in live and replayed TUI rendering and replace it with friendly context-management status.

Contract:

```json
{
  "decision": "compact | handoff | complete | needs_user_input",
  "checkpoint": "nonempty, at most 1200 UTF-8 bytes",
  "question": "null, or at most 512 UTF-8 bytes"
}
```

Validate the complete JSON at 2,048 bytes, deny unknown fields, and enforce question/decision consistency in Rust.

Prompt policy:

```text
Finish only the current atomic work and directly relevant validation; do not
start another milestone, broaden scope, spawn agents, compact, clear a thread,
or emit a proposed Plan.

Choose:
- compact: remaining work is short or tightly coupled and same-thread
  continuity materially helps;
- handoff: substantial or separable work remains and clean context materially
  helps;
- complete: the goal and relevant validation are finished;
- needs_user_input: a material user choice is still required.

Do not prefer either action merely because the checkpoint ran. If compact and
handoff are genuinely tied, choose compact. Return only the required JSON.
```

Behavior:

- `handoff`: enter existing Handoff Plan mode; only a later authoritative Plan can transfer.
- `compact`: invoke the configured existing compaction strategy—local, remote, remote-v2, or TokenBudget.
- `complete`: render the checkpoint and stop.
- `needs_user_input`: render the bounded question and stop without retry.
- A successful assessment with malformed, missing, unsupported, or oversized output defaults to `compact`.
- Assessment turn failure/interruption keeps current cancellation behavior.
- If Core already compacted during the assessment turn, treat a `compact` decision as satisfied and do not compact twice.
- True compaction failure preserves current behavior: show Core's error, leave the source idle, and perform no retry or handoff fallback.
- After a correlated compaction commits and its turn completes successfully, submit one fixed start-if-idle continuation turn in the same thread.
- A committed compaction followed by a post-hook failure remains committed but does not auto-continue.

### Existing compact RPC hardening

Keep `thread/compact/start`; do not add another compact endpoint.

- Add an idle-only Core submission path that reserves `active_turn` atomically and starts `CompactTask` without `spawn_task`'s `abort_all_tasks(Replaced)` behavior.
- Reject if another task won the race.
- Return the existing Core submission ID as `turnId`.
- Add an optional request source enum, defaulting to manual; automatic context management records automatic provenance.
- Track the exact returned turn ID. Accept only its `ContextCompaction` completion and terminal status.
- Never resend after transport uncertainty.
- Add an optional start-if-idle mode to `turn/start` so assessment and post-compaction continuation cannot steer another client's turn.
- Preserve current defaults for existing callers.

## Serial PR Plan

### 1. Atomic compaction and turn-start contracts

- [x] Harden `thread/compact/start`, return `turnId`, add provenance, and expose start-if-idle turn submission.
- [x] Update app-server v2 documentation and generated schemas.
- [x] Cover idle start, busy rejection without interruption, exact IDs, manual defaults, and remote-executor configurations.
- [x] Preserve compatibility for the Python SDK's older bundled runtime while capability-gating `startIfIdle`.
- [x] Open draft PR [#245](https://github.com/dkropachev/codex/pull/245).
- [ ] Resolve external CI blockers and merge.

### 2. Manual handoff: default and `--ask`

- [ ] Land the Plan-style facades, command parsing, attachment handling, authoritative-Plan fix, typed gates, fresh-thread proceed/stay flow, inherited context ceiling, and initial snapshots.
- [ ] Keep `/handoff` unavailable until this complete vertical behavior and its tests are present.

### 3. Deferred handoff and recovery

- [ ] Add `--defer`, pending display, one-time merge with the next model-bound prompt, local/shell-command preservation, destructive confirmations, navigation, replay, reconnect, and rejected-submission recovery.

### 4. Context-pressure guidance

- [ ] Add the 70% compact-or-handoff hint, compaction-ID watermark, strict compaction/new-thread rearming, and snapshots.
- [ ] Do not expose an automatic setting yet.

### 5. Automatic context management

- [ ] Add the renamed opt-in threshold, structured source-wrap-up assessment, compact/handoff/complete/question routing, correlated compaction, automatic continuation, bounded telemetry, replay/race handling, and live TUI coverage.

Each PR updates the feature spec only for behavior actually delivered in that PR.

## Test Plan

- Parser: `-x`, `--ask -x`, lone `-`, and `-- -x`.
- Manual flow: remote-only bare attachments, queued attachments, unsupported-image restoration, stale Plan after steer, revised Plan after steer, detached replay invalidation, settings preservation, and source resumability.
- Deferred flow: exact-once consumption, local and shell commands, navigation, disconnect, commit reconciliation, and every destructive confirmation.
- Guidance: 69/70 boundaries, cumulative-versus-active usage, unknown window, unseen versus replayed compaction IDs, background compaction, and new-thread reset.
- Assessment: every valid decision, malformed output defaulting to compact, strict ownership/generation, commentary versus final output, request-user-input, duplicate/stale events, and hidden/friendly replay rendering.
- Compaction: busy-race rejection, exact item/turn correlation, inline compaction during assessment, pre-commit failure, post-commit hook failure, transport uncertainty, no resend, and start-if-idle continuation.
- PTY tests: selected handoff reaches fresh execution and preserves the source; selected compact stays on the same thread, compacts exactly once, then continues.
- Add `insta` coverage for slash discovery, usage/gate errors, validation/cancellation messages, transfer failures, pending-awaiting-commit, all discard variants, selector decisions, compact success, and failure states.
- Remove production-only test helpers and static-value tests; drive assertions through notifications/events and test-local helpers.

## Review and Merge Gate

For every PR:

1. Run focused project tests, feature-spec verification, required schema generation, and snapshot review.
2. Run scoped `just fix -p …` and `just fmt` after tests, following repository ordering.
3. Open the PR and run review/fix round one with fresh Standards and Spec reviewers plus breaking-change, context, testing, and change-size checks.
4. Apply fixes and repeat validation.
5. Run review/fix round two with fresh reviewers. If round-two fixes change behavior, repeat fresh verification until it reaches zero findings.
6. Watch all required CI/CD checks and review threads. Do not merge while any required check is red or any actionable review thread remains.
7. Merge, update `master`, then rebase and begin the next PR.

The complete local Rust suite is run only after requesting approval, as required by repository instructions; CI must still be fully green before every merge.

## Assumptions

- Automatic context management remains opt-in.
- Manual `/compact` and `/handoff` retain explicit meanings.
- PR 1 is the smallest complete vertical contract; splitting Core, app-server, or SDK halves would land dormant APIs or temporarily break public compatibility.
- The unrelated local `custom_terminal::cursor` baseline failures are not folded into these PRs unless CI proves the branch causes them.
