# Plan Handoff

## Summary

Plan handoff safely transfers unfinished TUI work into a fresh session before the active context is
exhausted. It uses Plan-mode exploration and clarification to produce a bounded, decision-complete
handoff plan, preserves the source thread for later resume, and either starts execution in the new
thread or leaves the plan pending for the user's next model-bound prompt.

## Behavior

The TUI exposes `/handoff [--ask | --defer] [--] [guidance...]`. With no disposition option, a
successful handoff generates a plan, clears into a fresh thread using the existing clear-session
lifecycle, and automatically submits the plan for execution. `--ask` waits until a valid plan is
available, then offers `Clear and proceed`, `Clear and defer`, and `Stay in Handoff mode`; Escape is
equivalent to staying. `--defer` clears as soon as a valid plan is available, displays it as a
pending handoff in the fresh thread, and does not contact the model until the user next submits a
model-bound prompt.

`--ask` and `--defer` are mutually exclusive, and `--` ends option parsing. Unknown leading options
and invalid combinations show command usage without changing mode or thread. Guidance and composer
attachments are input to handoff planning only; they never request implementation in the source
thread. Shell-escape input is rejected rather than executed as handoff guidance. The command uses
the same collaboration-mode availability rules as `/plan`, and adding `/handoff` does not change
`/plan` parsing or behavior.

Handoff mode clones the effective Plan collaboration mask, retains `ModeKind::Plan` on the protocol
boundary, and is labeled `Handoff` in the TUI. Its instructions require the plan to record the goal,
completed work, current state, changed files, validation results, decisions, constraints, blockers,
ordered next steps, and acceptance criteria. The planner may explore and ask necessary
clarifications exactly as it does in Plan mode. If it determines that the requested work is already
complete, it reports completion without a `<proposed_plan>` and no transfer occurs. Handoff does not
introduce a new protocol collaboration mode.

Only the latest completed, nonempty, authoritative Plan item from the handoff turn may trigger a
transfer. The plan is capped at exactly 8 KiB of UTF-8 bytes. An empty or oversized plan, an
interrupted turn, or a failed plan generation leaves the source thread intact and gives the user a
bounded retry or manual-handoff hint.

A proceeding handoff uses the existing `/clear` session semantics. The old thread remains
resumable, while the new thread preserves the effective model, working directory, permissions,
configuration, and service tier. The first Default-mode turn receives only fixed execution
instructions and the bounded handoff plan from the old conversation; source history, clarification
messages, and handoff guidance are not copied into that turn. The plan remains recoverable in
process until the app server commits that exact execution prompt. A rejected or disconnected
submission falls back to a visible pending plan instead of silently losing execution.

A deferred handoff is visibly labeled as pending in the fresh thread but is not sent to the model
on its own. The next agent-bound user prompt submits that instruction together with the pending
plan, exactly once. Local slash commands and shell commands do not consume it. In-process thread
navigation preserves pending state. Clearing or deleting a thread with a pending handoff requires
confirmation. Pending state is intentionally not durable across process restart; the resumable
source thread and its finalized plan remain the recovery path.

The TUI always derives handoff context pressure from baseline-adjusted active-context use in
`last_token_usage`, matching the existing context display. Cumulative session usage is not used, and
an unknown context window produces neither a hint nor automatic handoff. At 70% used, the TUI emits
one non-model-visible hint recommending `/handoff`. That hint rearms only after compaction drops use
below 70% or a new thread starts.

Automatic handoff is disabled unless `[tui].auto_handoff_threshold_percent` is configured. The
accepted inclusive range is 71 through 85; values outside that range are configuration errors. A
live turn crossing the configured used-context threshold latches eligibility, but automation starts
only when the primary thread is idle in Default mode and has no queued input, modal, pending user
input request, rate-limit prompt, active goal continuation, parent-owned input, or active
descendants.

An eligible automatic handoff first submits one visible wrap-up task asking the agent to finish its
current atomic work safely, run relevant targeted validation, stop expanding scope, and record any
blockers. Only successful wrap-up completion enters Handoff mode and requests a focused plan. A
valid bounded plan then clears into a fresh thread and begins execution automatically. Configuration,
Workflow, Plan, Handoff, and side-session states suppress recursive automation; eligibility may be
reconsidered after the TUI returns to an idle Default session.

Usage replay while resuming a thread may display the 70% hint but never starts automation. The first
later live completion may qualify. Duplicate live usage or completion events cannot trigger more
than one sequence. A failure, interruption, empty or oversized plan, or compaction-driven drop below
the configured threshold before automation begins cancels that automatic transfer without retrying
or clearing the source thread.

Handoff telemetry uses bounded counters for manual and automatic triggers, user disposition,
completion, cancellation, and failure. Prompt, guidance, plan, filename, and other user-authored text
must never be recorded in counter names, values, or attributes.

## Entry Points

- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/slash_command.rs](../tui/src/slash_command.rs)
- [codex-rs/tui/src/collaboration_modes.rs](../tui/src/collaboration_modes.rs)
- [codex-rs/tui/src/chatwidget](../tui/src/chatwidget)
- [codex-rs/tui/src/app](../tui/src/app)
- [codex-rs/tui/src/token_usage.rs](../tui/src/token_usage.rs)
- [codex-rs/config/src/types.rs](../config/src/types.rs)
- [codex-rs/core/src/config/mod.rs](../core/src/config/mod.rs)
- [codex-rs/core/config.schema.json](../core/config.schema.json)

## Subfeatures

### Manual Transfer

#### Entry Points

- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/slash_command.rs](../tui/src/slash_command.rs)
- [codex-rs/tui/src/chatwidget](../tui/src/chatwidget)
- [codex-rs/tui/src/app](../tui/src/app)

#### Invariants

- No manual disposition clears the source thread before a valid, bounded plan exists.
- `Stay in Handoff mode`, including Escape from the disposition prompt, never clears or submits the
  plan.
- A completed-work response without an authoritative Plan item remains in the source thread.
- Fresh-session execution inherits effective runtime settings but not source-thread conversation
  history.

### Deferred Execution

#### Entry Points

- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/chatwidget](../tui/src/chatwidget)
- [codex-rs/tui/src/app](../tui/src/app)

#### Invariants

- Merely displaying, navigating away from, or returning to a pending handoff is never model-bound.
- A pending plan is consumed once, and only by the next agent-bound user prompt.
- Destructive session actions require confirmation while a pending handoff exists.
- Loss of process-local pending state never makes the source thread or finalized plan
  unresumable.

### Context Guidance and Automation

#### Entry Points

- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/token_usage.rs](../tui/src/token_usage.rs)
- [codex-rs/tui/src/chatwidget](../tui/src/chatwidget)
- [codex-rs/config/src/types.rs](../config/src/types.rs)
- [codex-rs/core/src/config/mod.rs](../core/src/config/mod.rs)

#### Invariants

- Hinting and automation use baseline-adjusted `last_token_usage`, never cumulative session usage.
- Replayed usage cannot initiate automatic work, and each live threshold crossing starts at most one
  automatic sequence.
- Automation cannot bypass queued user intent, an interaction surface, active goal work, parent-owned
  input, or descendant work.
- Every automatic sequence completes the visible wrap-up task before planning and validates the
  handoff plan before clearing.
- Any automatic cancellation or failure leaves the source thread intact and does not retry on its
  own.

## Invariants

- Plan handoff is TUI-only and adds neither a top-level `codex handoff` command nor app-server API.
- Handoff uses `ModeKind::Plan` and the existing clear lifecycle; it does not add protocol mode or
  session-lifecycle variants solely to transfer a plan.
- The model-visible handoff payload is nonempty valid UTF-8, at most 8,192 bytes, and contains no
  unbounded source transcript fragment.
- Only a finalized authoritative Plan item can initiate transfer, and the latest revision wins.
- The source thread remains resumable across proceed, defer, automatic transfer, and all failure
  paths.
- Deferred plans remain invisible to the model until merged with an agent-bound user prompt.
- The 70% hint is fixed, while automatic transfer remains opt-in and accepts only thresholds from
  71% through 85% inclusive.
- Manual and automatic handoff never bypass normal sandbox, approval, permission, or service-tier
  behavior.
- Handoff metrics have bounded cardinality and never include user or model content.
- Existing `/plan` behavior remains unchanged.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Plan handoff orchestration and model submission are owned by the TUI and reuse the existing core
agent turn contract without changing core agent behavior.

#### Status

Not covered

### app-server-api (app-server API behavior)

#### Description

Plan handoff is TUI-only and introduces no app-server request, response, notification, or protocol
surface.

#### Status

Not covered

### cli (main CLI command behavior)

#### Description

Plan handoff adds a TUI slash command, not a top-level CLI subcommand.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise session transfer across real terminal interaction, including the
automatic sequence and recovery of the old thread.

#### Test cases

- Default manual handoff plans, clears, starts a fresh thread without source history, executes, and leaves the source thread resumable: codex-rs/tui/tests/suite/plan_handoff__live.rs:plan_handoff_default_transfers_to_fresh_thread_and_source_remains_resumable
- Deferred handoff waits through a local command, then merges the first later agent-bound prompt exactly once without source history: codex-rs/tui/tests/suite/plan_handoff__live.rs:plan_handoff_deferred_waits_for_next_agent_prompt_and_merges_it_once
- A live threshold crossing runs wrap-up, handoff planning, clear, and first-turn execution exactly once: codex-rs/tui/tests/suite/plan_handoff__live.rs:plan_handoff_live_threshold_runs_wrap_up_plan_and_fresh_execution

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise command parsing, handoff planning and validation, disposition
UI, pending-plan state, context-pressure latches, safety gates, telemetry, and rendered hints.

#### Test cases

- Disposition and guidance parsing is covered: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:parses_disposition_and_guidance
- The option terminator permits option-like guidance: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:option_terminator_makes_option_like_text_guidance
- Conflicting and unknown leading options are rejected: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:rejects_conflicting_and_unknown_leading_options
- Guidance offsets and generated prompts preserve UTF-8 byte positions: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:guidance_offset_is_a_byte_offset_and_prompt_ends_with_guidance
- The Handoff mask preserves Plan settings and adds handoff requirements: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:handoff_mask_preserves_plan_settings_and_appends_requirements
- The slash-command registry uses canonical handoff availability and argument rules: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:slash_command_registry_matches_plan_availability
- Pending plans preserve source text and generate bounded fresh-thread instructions: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:pending_plan_preserves_text_and_formats_fresh_execution
- Exact UTF-8 plan byte bounds are enforced: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:validates_exact_utf8_byte_boundary
- The default command submits the planning prompt in protocol Plan mode while displaying Handoff mode: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_default_command_submits_planning_prompt_in_displayed_handoff_plan_mode
- Inline dispositions, guidance, and `--` are dispatched into the intended planning prompt: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_inline_options_and_double_dash_shape_only_the_planning_prompt
- Unknown and conflicting disposition options report usage without a model submission: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_unknown_and_conflicting_options_show_usage_without_submission
- Leading-bang guidance is model-bound literally and never runs as a shell command: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_leading_bang_guidance_is_sent_literally_and_never_run_as_shell
- Composer attachments and text elements survive handoff guidance rewriting: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_composer_submission_preserves_guidance_attachment_and_text_element
- Non-Default mode rejects handoff without changing existing `/plan` dispatch: codex-rs/tui/src/chatwidget/tests/plan_handoff__commands.rs:plan_handoff_command_is_rejected_outside_default_mode_without_changing_plan_behavior
- Completed default and deferred plans emit their requested transfer disposition: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_completed_default_and_deferred_plans_emit_the_requested_transfer
- The `--ask` decision renders all three choices at normal and narrow widths and Escape stays in Handoff mode: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_ask_plan_renders_three_choices_wide_and_narrow_and_escape_stays
- The `--ask` proceed, defer, and explicit-stay callbacks emit the intended source-scoped events: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_ask_choices_emit_proceed_defer_and_explicit_stay
- Exact 8 KiB multibyte plans transfer, exact 8,193-byte plans fail, and the latest revision wins: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_plan_bounds_and_latest_revision_control_transfer
- Completion without a Plan item and empty Plan output do not transfer or leave Handoff mode: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_missing_and_empty_plans_leave_source_thread_without_transfer
- Deferred plans display locally, ignore local and shell commands, merge with the next model prompt, and are consumed once: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_deferred_plan_waits_through_local_and_shell_commands_then_merges_prompt
- A failed fresh-thread start leaves the source thread and transcript intact without unsubscribing it: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_fresh_start_failure_keeps_source_thread_and_ui_intact
- The final transfer gate rechecks active descendants before starting or clearing a destination: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_final_transfer_gate_rechecks_active_descendants
- Deferred transfer installs no model turn and restores its runtime-only plan after direct in-process reattachment: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_defer_installs_runtime_plan_and_restores_it_on_direct_resume
- Proceed preserves effective model, reasoning, service tier, working directory, and permissions, retains the plan until the exact first fresh turn is committed, and leaves the source resumable: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_proceed_preserves_settings_and_submits_first_fresh_turn
- The 70% hint uses active rather than cumulative usage, fires at the boundary, deduplicates, and rearms after a drop: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_context_hint_uses_active_usage_at_70_percent_and_rearms_after_drop
- Unknown context windows do not hint or automate, and replayed high usage does not latch automation: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_unknown_window_has_no_hint_and_replayed_usage_does_not_latch_auto
- A live automatic threshold crossing submits wrap-up in Default mode before handoff planning in Plan mode: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_live_threshold_completion_starts_wrap_up_then_handoff_planning
- ChatWidget-local queued input, interaction surfaces, rate limits, active goals, modes, side sessions, cancellation, and duplicate starts block or cancel automatic handoff: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_automatic_local_blockers_compaction_and_cancellation_are_single_shot
- Replayed low usage and compaction preserve live automation state, while mode changes and navigation cancel active automatic phases without relatching: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_replay_mode_change_and_navigation_do_not_restart_automatic_handoff
- Automatic wrap-up ignores unrelated completion, and the queued planning transition rechecks blockers that appear in the event gap: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_automatic_ignores_stale_completion_and_rechecks_planning_gap
- The synchronous turn-start response owns the handoff even when an older identical user item is delivered later: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_turn_start_response_beats_identical_stale_user_item
- A lost turn-start response cannot reconcile a new bare handoff against an older identical prompt before its submission boundary: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_lost_start_response_does_not_reuse_older_identical_plan
- An accepted handoff steer rebinds ownership to the running turn instead of leaving completion orphaned: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_accepted_steer_rebinds_the_running_owned_turn
- A response-lost accepted steer is reconciled inside its existing running turn rather than searched only after it: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_response_lost_steer_reconciles_inside_the_running_turn
- A rejected steer queued at completion cancels automatic handoff before the owned-turn guard can defer it: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_rejected_steer_queue_cancels_before_ownership_check
- Live compaction preserves an above-threshold latch, cancels after a real drop below the automatic threshold, and permits a later live crossing: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_compaction_preserves_high_latch_and_rearms_after_real_drop
- Clearing a rate-limit prompt reissues the latched automatic candidate without waiting for unrelated input: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_rate_limit_unblock_reissues_the_latched_candidate
- Replayed low usage rearms the one-shot 70% hint without latching automatic work: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_replayed_low_usage_rearms_the_one_shot_hint_only
- Stay rotates the transaction generation so stale decision callbacks cannot transfer an older plan: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_stay_rotates_generation_and_rejects_stale_transfer
- In-process reattachment preserves the generation counter so a cancelled transaction ID is never reused: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_reattach_does_not_reuse_a_cancelled_generation
- A manual plan completed while detached is reconciled by its exact submitted prompt and transferred after replay: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_reconciles_manual_plan_completed_while_detached
- A rejected proceeding execution becomes a visible pending plan instead of being lost: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_proceed_rejection_restores_a_visible_pending_plan
- Disconnect recovery turns an uncommitted Proceed into one pending retry without wrapping the execution prompt twice: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_disconnect_does_not_expand_proceed_prompt_twice
- Reconnect consumes a proceeding plan whose exact prompt committed before disconnect even after uncertain-submit recovery cleared its marker: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_reconnect_consumes_proceed_committed_before_disconnect
- A failed collaboration-mode update exits Handoff cleanly to Default and leaves the source retryable: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_mode_update_failure_returns_manual_handoff_to_default
- An attachment-only clarification submission is owned through the turn-start response and can finalize the handoff plan: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_attachment_only_clarification_owns_its_turn
- The misalignment precaution routes New chat through pending-plan confirmation instead of discarding immediately: codex-rs/tui/src/chatwidget/tests/plan_handoff__races.rs:plan_handoff_misalignment_new_chat_requests_pending_confirmation
- App-level automatic candidates reject non-primary sessions, overlays, pending user input, parent-owned input, and active descendants, while an eligible candidate starts wrap-up: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_automatic_candidate_obeys_app_safety_gates
- The app-owned planning gate rechecks overlays after wrap-up and cancels without submitting planning: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_automatic_planning_gate_rechecks_app_owned_blockers
- A descendant that closes without another completion event reissues the latched candidate: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_closed_descendant_reissues_the_latched_candidate
- Descendant-store lock contention delays the app gate instead of being misclassified as active work and cancelling: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_descendant_lock_contention_waits_instead_of_cancelling
- Root reattachment preserves one-shot hint and automatic no-retry state without restoring a stale latch: codex-rs/tui/src/app/tests/plan_handoff__session_transfer.rs:plan_handoff_root_reattach_preserves_passive_hint_and_no_retry_state
- Deferred pending state survives in-process thread-input capture and restore and redisplays without a model turn: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_deferred_plan_survives_thread_input_state_capture_and_restore
- Bare and named clear/new actions require explicit pending-plan discard confirmation, and delete warns before acting: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_pending_plan_confirms_clear_and_new_and_warns_before_delete
- Failed and interrupted manual planning remains retryable, while interrupted and oversized automatic planning returns to Default without transfer or retry: codex-rs/tui/src/chatwidget/tests/plan_handoff__state.rs:plan_handoff_failed_and_interrupted_planning_never_transfer_and_remain_retryable
- Handoff telemetry enumerates bounded counter names and enum-derived attributes without user-authored content: codex-rs/tui/src/handoff/plan_handoff__command_tests.rs:telemetry_dimensions_are_bounded_and_content_free

### login-auth (auth and login behavior)

#### Description

Plan handoff does not change authentication, credential selection, token refresh, or account
storage.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Plan handoff is not exposed as a Codex-as-MCP-server tool.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Plan handoff does not change MCP client transport, discovery, OAuth, or cleanup behavior.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Plan handoff reuses normal model turns and does not change the lower-level Codex API client or wire
protocol.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Plan handoff is interactive TUI behavior and does not change non-interactive exec semantics.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Handoff counter emission is covered at the TUI component boundary and does not change telemetry
export routing or OTLP behavior.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Plan handoff does not change exec-server process, filesystem, HTTP, relay, or WebSocket behavior.

#### Status

Not covered

## Test Generation Notes

Generate parser tests for empty guidance, whitespace, each flag order, the `--` sentinel, option-like
guidance after the sentinel, mutually exclusive flags, unknown leading options, text elements,
local and remote attachments, and shell-escape attempts. Regression tests must prove `/plan` keeps
its existing behavior and handoff is unavailable wherever the Plan collaboration mask is
unavailable.

Generate state-machine and snapshot tests for the default path and each `--ask` disposition,
including Escape, narrow terminals, repeated or revised Plan items, a completion response with no
plan, interruption, planner failure, and empty output. Exercise 8,191, 8,192, and 8,193-byte plans
as well as a multibyte character straddling the limit; reject without truncating or emitting invalid
UTF-8. Verify that retry messages are bounded and that failed validation never clears the source
thread.

Deferred tests should assert that rendering the pending plan causes no model request, local slash
commands and shell commands leave it pending, the next agent-bound prompt carries fixed execution
instructions plus the plan and new instruction exactly once, thread navigation retains it, and
clear/delete confirmation can either preserve or discard it. Restart tests should treat pending
state as intentionally ephemeral and prove the source rollout remains resumable.

Context tests should compute usage from `last_token_usage` with the same baseline adjustment as the
status UI. Cover 69%/70% hint boundaries, 70%/71% and 85%/86% configuration validation boundaries,
unknown windows, misleading cumulative totals, post-compaction rearming, new-thread rearming,
replayed resume events, the first later live completion, and duplicate usage/completion delivery.

Automatic-sequence tests should separately exercise every idle gate and every suppressed mode, then
cover the successful order: live crossing, visible wrap-up, successful completion, focused handoff
plan, clear/new thread, and first execution turn. Assert that wrap-up wording stops scope growth and
requests targeted validation and blockers. Failures, interruptions, invalid plans, and a usage drop
before start must cancel without clearing or retrying.

The model-visible handoff plan can exceed 1,000 tokens even though it is capped at 8 KiB. Treat any
implementation or later expansion of that payload as requiring explicit manual context-safety
review, and generate tests that prove the byte cap is enforced before the payload enters model
context. The review must confirm that the plan is a bounded explicit first-turn user message or,
if it is injected as context instead, uses the repository's contextual-user-fragment path.

Focused configuration tests belong beside the public TUI config types and core config loading tests;
they should cover omission and the exact inclusive `71..=85` range, followed by config-schema
regeneration. Telemetry tests should enumerate the bounded manual/automatic trigger, disposition,
completion, cancellation, and failure dimensions and reject prompt, plan, attachment, and filename
content in emitted attributes.
