# Context Management

## Summary

Context management keeps long-running threads usable without allowing one client to replace another client's active work. It defines atomic app-server contracts for standalone compaction and idle-only turn starts, plus a manual TUI handoff to a fresh thread.

## Behavior

`thread/compact/start` starts only after Core atomically reserves an idle thread. It returns Core's exact `turnId`; a busy thread returns an invalid-request error without interruption. Its optional `source` defaults to `manual`; `automaticContextManagement` selects the automatic trigger for hooks and metrics, while model-backed strategies also carry it in request metadata and compaction analytics.

`turn/start` retains start-or-steer by default. With `startIfIdle: true`, it starts only on an idle thread and otherwise rejects without recording, queueing, or steering the input. Neither RPC retries a rejected or transport-uncertain submission.

`/handoff` enters a Handoff presentation of Plan mode from an idle, resumable Default-mode thread. It accepts optional `--ask` and guidance. A completed Plan item owned by the planning turn is eligible only while no later accepted user input has superseded it. Default handoff starts a fresh Default-mode thread and submits only the validated plan as new context; `--ask` offers proceed or stay. The source remains resumable. The full generated prompt obeys the existing 10,000 estimated-token model-context item ceiling. Manual `/compact` retains its explicit compaction meaning.

## Entry Points

- [codex-rs/protocol/src/turn_input.rs](../protocol/src/turn_input.rs)
- [codex-rs/core/src/codex_thread.rs](../core/src/codex_thread.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/thread.rs](../app-server-protocol/src/protocol/v2/thread.rs)
- [codex-rs/app-server/src/request_processors/thread_processor.rs](../app-server/src/request_processors/thread_processor.rs)
- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/chatwidget/handoff.rs](../tui/src/chatwidget/handoff.rs)
- [codex-rs/tui/src/app/handoff.rs](../tui/src/app/handoff.rs)

## Subfeatures

### Manual Handoff

#### Entry Points

- [codex-rs/tui/src/chatwidget/slash_dispatch.rs](../tui/src/chatwidget/slash_dispatch.rs)
- [codex-rs/tui/src/chatwidget/handoff.rs](../tui/src/chatwidget/handoff.rs)
- [codex-rs/tui/src/app/handoff.rs](../tui/src/app/handoff.rs)

#### Invariants

- A bare live or queued command retains local and remote attachments; unsupported images restore the original command.
- A same-turn accepted steer or replayed user message invalidates an earlier Plan, while a revised completed Plan can become authoritative.
- The app rechecks source ownership, generation, pending interactions, and active descendants before starting a fresh thread.
- The app verifies the source's latest persisted turn before and after fresh-thread creation; unavailable turn history pauses transfer. A concurrent external-client update after the final read is outside this TUI-side check.
- Failed validation or transfer leaves the source thread resumable; `--ask` can stay in Handoff mode without transferring.

## Invariants

- Admission is decided inside Core, not from a stale app-server status snapshot, and a losing request cannot abort or steer the winning turn.
- Returned IDs match lifecycle events, omitted provenance is manual, and every strategy exposes automatic provenance through its supported observability surfaces.
- App-server and exec-server may run on different supported operating systems.
- A handoff destination receives the bounded plan prompt without copying source transcript history, and preserves the selected model, Default-mode reasoning effort, working directory, permissions, and service tier.
- The handoff plan is sent as the fresh thread's initial user turn, with a 10,000 estimated-token cap on the complete prompt. It is not inserted as a Core context fragment.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Core coverage proves atomic reservation, exact IDs, automatic provenance, and non-destructive busy rejection.

#### Test cases

- Concurrent idle compactions admit exactly one turn, preserve its exact ID, and reject replacement: codex-rs/core/tests/suite/compact.rs:idle_only_compaction_is_atomic_and_reports_its_exact_turn_id
- Concurrent idle user starts admit exactly one turn and reject the loser without steering: codex-rs/core/tests/suite/turn_input_submission.rs:user_idle_start_is_atomic_for_concurrent_submissions
- Streamed remote-v2 standalone compaction preserves automatic trigger/reason metadata: codex-rs/core/tests/suite/compact_remote.rs:standalone_automatic_compaction_preserves_remote_provenance
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
- Idle-only turn start preserves an empty user turn in Plan mode: codex-rs/app-server/tests/suite/v2/turn_start.rs:turn_start_with_empty_input_runs_model_request
- Omission preserves start-or-steer behavior: codex-rs/app-server/tests/suite/v2/turn_start.rs:turn_start_defaults_to_start_or_steer_while_active

### cli (main CLI command behavior)

#### Description

No top-level CLI surface changes.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

PTY coverage exercises a manual handoff through Plan generation, fresh execution, and source resume.

#### Test cases

- Default handoff executes in a fresh thread without copying source-only text and the source remains resumable: codex-rs/tui/tests/suite/context_management__handoff_live.rs:plan_handoff_default_transfers_to_fresh_thread_and_source_remains_resumable

### tui-component (focused TUI component behavior)

#### Description

Component coverage exercises parser, attachments, Plan authority, ask/stay decisions, gates, settings carryover, and failure restoration.

#### Test cases

- Leading option-like guidance is rejected while lone dash and `-- -x` remain literal: codex-rs/tui/src/handoff_tests.rs:parser_rejects_leading_option_like_guidance,parser_preserves_lone_dash_and_option_like_guidance_after_separator
- The invalid-option error has a rendered usage snapshot: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:leading_hyphen_guidance_is_rejected_before_submission
- The generated execution prompt obeys the inherited context-item ceiling: codex-rs/tui/src/handoff_tests.rs:handoff_fragment_obeys_existing_model_context_item_ceiling
- Live and queued bare commands preserve remote-only attachments, remote workspace image preparation retains handoff ownership, unsupported images restore the command, and protected source states reject and restore it: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:bare_live_handoff_keeps_remote_only_attachment,bare_queued_handoff_keeps_remote_only_attachment,remote_workspace_image_preparation_keeps_handoff_ownership,unsupported_handoff_image_restores_the_original_command,protected_source_states_reject_handoff_and_restore_the_command
- An accepted steer invalidates an earlier Plan in live and replayed flows: codex-rs/tui/src/chatwidget/tests/plan_handoff_authority.rs:accepted_user_input_invalidates_earlier_plan_in_live_and_replay_flows
- Default and ask dispositions require an owned completed Plan; ask can stay without transferring: codex-rs/tui/src/chatwidget/tests/plan_handoff_state.rs:completed_default_handoff_emits_fresh_thread_transfer,ask_handoff_stay_keeps_source_mode_and_invalidates_old_transfer,accepted_same_turn_steer_prevents_transfer_of_earlier_plan
- Fresh execution preserves source and selected settings, while an active descendant, accepted steer, source revision change, newer persisted turn, unavailable turn page, or failed start retains the source: codex-rs/tui/src/app/tests/plan_handoff_transfer.rs:manual_handoff_starts_fresh_execution_and_preserves_source,active_descendant_pauses_transfer_and_preserves_source,accepted_steer_notification_invalidates_earlier_handoff_plan,source_revision_change_during_start_pauses_transfer,newer_persisted_source_turn_pauses_transfer,unavailable_source_turn_page_pauses_transfer,fresh_start_failure_keeps_the_source_thread_visible,handoff_thread_start_failure_keeps_source_and_does_not_execute

### login-auth (auth and login behavior)

#### Description

Authentication behavior is unchanged.

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

Race two starts, correlate all notifications by returned ID, cover omitted/null/automatic sources, exercise each compaction strategy, and prove transport uncertainty never causes a resend. For manual handoff, exercise bare and queued attachments, same-turn steer invalidation, decision prompts, transfer gates, the complete execution payload, and source resumption.
