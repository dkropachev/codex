# Handoff

## Summary

Handoff moves an ongoing task into a fresh TUI thread using a focused, authoritative plan. The
source thread remains resumable. Users can transfer immediately, decide after planning, defer
execution until their next prompt, or opt into automatic handoff at high active-context usage.

## Behavior

`/handoff [--ask | --defer] [--] [guidance...]` starts from an idle, resumable Default-mode
session with collaboration modes enabled. It enters Handoff, a presentation of Plan mode, and
submits a planning prompt in the source thread. Guidance shapes the plan rather than authorizing
more implementation. Options are parsed only before guidance; `--` ends option parsing, a lone
`-` is literal guidance, and unknown leading options or combined `--ask --defer` are rejected
with usage text before submission. Repeating the same disposition is accepted. The planner is
instructed to explore and clarify, preserve completed work,
exclude secrets and irrelevant history, and include the goal, acceptance criteria, current state,
changed files, validation, decisions, constraints, blockers, and concrete next steps.

Only a completed authoritative Plan item from a successful handoff-owned turn can authorize
transfer. Ordinary assistant prose is insufficient. Accepted user input invalidates an earlier
Plan; a later completed Plan can establish new authority. Empty or oversized plans are rejected.
If work is complete, the planner is instructed to report completion without a proposed plan.

The default disposition starts a fresh Default-mode thread after planning. `--ask` shows a
proceed-or-stay decision without starting a destination early. Staying keeps manual Handoff mode
available for further planning and invalidates the old transfer action. Cancelling the decision
prompt has the same effect as staying. `--defer` returns to Default mode with a pending plan; the next model-bound prompt triggers a fresh thread containing
the plan and that prompt. Local and shell commands leave the plan pending.

Fresh execution receives the validated plan with an execution preamble, without copying the
source conversation history. Deferred execution also includes the next prompt and its attachments.
The complete generated text, including preamble and any deferred follow-up, must fit within
10,000 estimated tokens. The destination preserves the selected model, Default-mode reasoning
effort, working directory, permissions, and service tier.

Transfer checks source ownership, transaction generation, idle state, pending interactions, and
active descendants. It verifies the latest persisted source turn before and after destination
creation. An unavailable or changed source revision pauses transfer. These checks are best effort:
there is no atomic cross-thread transaction with other clients, and another client can change
the source after the final read. A failed final check can leave a created destination without
submitting its execution turn. If attachment fails before leaving the source, retain the source
view and roll back the transfer state. If attachment fails after leaving it, report the error
with instructions to resume the saved source thread.

Automatic handoff is disabled by default. Setting `[tui].auto_handoff_threshold_percent` to an
integer from 71 through 85 enables it. A live Default-mode turn at or above the threshold can
qualify for automation after completion. Usage is measured against the adjusted active-context
window, not cumulative session usage. Replay alone cannot trigger automation. The displayed
primary thread must pass the same safety gates. Automation first submits a visible wrap-up turn
in Default mode, then enters Handoff planning and uses the verified fresh-thread transfer.

The passive 70% context-pressure hint and explicit `/compact` behavior belong to
[context management](context-management.md). Realtime voice delegation and Guardian root-context
selection are separate features despite also using the word handoff.

## Entry Points

- [codex-rs/tui/src/handoff.rs](../tui/src/handoff.rs)
- [codex-rs/tui/src/chatwidget/handoff.rs](../tui/src/chatwidget/handoff.rs)
- [codex-rs/tui/src/chatwidget/handoff_deferred.rs](../tui/src/chatwidget/handoff_deferred.rs)
- [codex-rs/tui/src/chatwidget/handoff_automatic.rs](../tui/src/chatwidget/handoff_automatic.rs)
- [codex-rs/tui/src/app/handoff.rs](../tui/src/app/handoff.rs)
- [codex-rs/tui/src/app/handoff_deferred.rs](../tui/src/app/handoff_deferred.rs)
- [codex-rs/core/src/config/mod.rs](../core/src/config/mod.rs)

## Subfeatures

### Manual Planning and Decisions

#### Entry Points

- [codex-rs/tui/src/chatwidget/slash_dispatch.rs](../tui/src/chatwidget/slash_dispatch.rs)
- [codex-rs/tui/src/chatwidget/handoff.rs](../tui/src/chatwidget/handoff.rs)

#### Invariants

- A bare live or queued command retains local and remote attachments and text-element ranges.
- Unsupported images or failed admission restore the command for review.
- Parent-owned input, ephemeral sessions, busy or queued work, pending interactions, and active
  goals block admission. Finish or pause an active goal before handoff.
- A manual planning turn without an authoritative plan leaves the user in Handoff mode without
  transferring. A rejected Handoff mode update returns to Default mode when available and
  reports the failure.

### Deferred Execution and Recovery

#### Entry Points

- [codex-rs/tui/src/chatwidget/handoff_deferred.rs](../tui/src/chatwidget/handoff_deferred.rs)
- [codex-rs/tui/src/app/handoff_deferred.rs](../tui/src/app/handoff_deferred.rs)

#### Invariants

- The pending plan survives thread navigation, replay, and an in-process app-server reconnect;
  it does not execute merely because the thread is redisplayed. Pending state is held in
  process; this contract does not promise restoration after restarting the TUI.
- A later accepted source message invalidates the pending plan.
- Discarding commands (`/new`, `/clear`, `/fork`, `/worktree`, `/compact`, `/handoff`,
  `/archive`, `/delete`, `/quit`, `/exit`, `/logout`) ask for confirmation. Corresponding App
  actions and editing an earlier prompt use the same confirmation; immediate forced exit is
  exempt. Keeping or cancelling the dialog preserves the pending plan.
- If returning to Default mode is rejected, clear the pending plan, restore any in-flight prompt,
  and report the failure so the user can select Default mode and retry.
- A failed fresh start restores the next prompt while retaining the pending plan. A rejected
  initial destination turn restores the combined plan and prompt for review without resending.
- Follow-up attachments survive transfer, with text-element offsets adjusted for the plan prefix.

### Automatic Context Handoff

#### Entry Points

- [codex-rs/tui/src/chatwidget/handoff_automatic.rs](../tui/src/chatwidget/handoff_automatic.rs)
- [codex-rs/tui/src/app/handoff.rs](../tui/src/app/handoff.rs)

#### Invariants

- The threshold is optional; values outside 71 through 85 fail configuration validation.
- Unknown active-context usage cannot qualify a thread. A live drop below threshold clears an
  idle candidate.
- A failed or interrupted automatic step clears automation and returns from Handoff to Default
  mode when available. Missing or invalid authoritative plans also cancel automation. Manual
  user input during wrap-up cancels automation so it cannot take over the new request.
- Cancellation suppresses immediate retries. A subsequent live below-threshold usage update
  while no handoff is active rearms automation; later qualifying usage may start a new cycle.
- A user-selected mode change leaves Handoff and invalidates the active transaction.
- Compaction followed by live usage below threshold cancels automation before planning begins;
  a replayed compaction is insufficient. Once planning has begun, that cancellation rule no
  longer applies.

## Invariants

- The source remains resumable; source history is neither replaced nor copied into the fresh
  execution prompt. This does not roll back repository changes made during the source session.
- Each transfer must match the current source, generation, completed plan turn, and plan text.
- New input, stale callbacks, or unverifiable source history cannot silently authorize old plans.
- The generated prompt is the destination's initial user turn, not a Core-injected context fragment.
- Remote workspaces and different supported app-server/exec-server operating systems remain usable.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Handoff orchestration belongs to the TUI. It reuses existing Plan and user-turn behavior without
adding a Core handoff operation; shared Core admission coverage belongs to context management.

#### Status

Not covered

### app-server-api (app-server API behavior)

#### Description

Handoff uses existing thread start, read, turn paging, and turn submission RPCs. There is no
handoff-specific RPC; source revision checks and recovery are covered through TUI components.

#### Status

Not covered

### cli (main CLI command behavior)

#### Description

The entry point is a TUI slash command, with no new top-level CLI command or flag.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

Default and automatic transfer terminal coverage is currently declared by context management,
whose feature-prefixed tests retain their ownership. Dedicated handoff coverage should exercise
interactive decisions and delayed execution across a running terminal session.

#### Test cases

- Ask can stay and later proceed through a newly completed plan in a running terminal: missing
- Deferred handoff leaves local commands in the source and transfers on the next model-bound prompt in a running terminal: missing

### tui-component (focused TUI component behavior)

#### Description

Component coverage exercises parser, attachments, Plan authority, ask/stay decisions, gates, settings carryover, and failure restoration.

#### Test cases

- Leading option-like guidance is rejected while lone dash and `-- -x` remain literal: codex-rs/tui/src/handoff_tests.rs:parser_rejects_leading_option_like_guidance,parser_preserves_lone_dash_and_option_like_guidance_after_separator
- Guidance offsets remain correct after option parsing for attached command content: codex-rs/tui/src/handoff_tests.rs:parser_retains_guidance_offset_for_attached_command_content
- Planning uses the Handoff Plan mask and guidance does not authorize implementation: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:default_handoff_submits_planning_prompt_in_handoff_plan_mode,ask_and_literal_option_guidance_shape_only_the_planning_prompt
- Slash discovery renders the handoff command: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:handoff_appears_in_slash_discovery
- Guidance preserves local images and shifted text-element ranges: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:handoff_guidance_keeps_local_image_and_text_element
- The invalid-option error has a rendered usage snapshot: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:leading_hyphen_guidance_is_rejected_before_submission
- The generated execution prompt obeys the inherited context-item ceiling: codex-rs/tui/src/handoff_tests.rs:handoff_fragment_obeys_existing_model_context_item_ceiling
- Live and queued bare commands preserve remote-only attachments, remote workspace image preparation retains handoff ownership, unsupported images restore the command, and protected source states reject and restore it: codex-rs/tui/src/chatwidget/tests/plan_handoff_commands.rs:bare_live_handoff_keeps_remote_only_attachment,bare_queued_handoff_keeps_remote_only_attachment,remote_workspace_image_preparation_keeps_handoff_ownership,unsupported_handoff_image_restores_the_original_command,protected_source_states_reject_handoff_and_restore_the_command
- An accepted steer invalidates an earlier Plan in live and replayed flows: codex-rs/tui/src/chatwidget/tests/plan_handoff_authority.rs:accepted_user_input_invalidates_earlier_plan_in_live_and_replay_flows
- A newer empty completed Plan replaces earlier authority rather than falling back to the old plan: codex-rs/tui/src/chatwidget/tests/plan_handoff_authority.rs:empty_completed_plan_replaces_earlier_authority
- Ask shows a decision before transfer and empty Plans produce visible validation errors: codex-rs/tui/src/chatwidget/tests/plan_handoff_state.rs:ask_handoff_shows_proceed_or_stay_without_transferring_early,empty_handoff_plan_shows_validation_error_without_transfer
- Rejected Handoff mode updates restore Default mode: codex-rs/tui/src/chatwidget/tests/plan_handoff_state.rs:rejected_handoff_mode_update_restores_default_mode
- Deferred execution bounds the complete plan and follow-up text together: codex-rs/tui/src/handoff_tests.rs:deferred_handoff_bounds_the_combined_plan_and_followup
- Default and ask dispositions require an owned completed Plan; ask can stay without transferring: codex-rs/tui/src/chatwidget/tests/plan_handoff_state.rs:completed_default_handoff_emits_fresh_thread_transfer,ask_handoff_stay_keeps_source_mode_and_invalidates_old_transfer,accepted_same_turn_steer_prevents_transfer_of_earlier_plan
- Fresh execution preserves source and selected settings, while an active descendant, accepted steer, source revision change, newer persisted turn, unavailable turn page, or failed start retains the source: codex-rs/tui/src/app/tests/plan_handoff_transfer.rs:manual_handoff_starts_fresh_execution_and_preserves_source,active_descendant_pauses_transfer_and_preserves_source,accepted_steer_notification_invalidates_earlier_handoff_plan,source_revision_change_during_start_pauses_transfer,newer_persisted_source_turn_pauses_transfer,unavailable_source_turn_page_pauses_transfer,fresh_start_failure_keeps_the_source_thread_visible,handoff_thread_start_failure_keeps_source_and_does_not_execute
- Deferred parsing accepts `--defer` and rejects conflicting dispositions: codex-rs/tui/src/handoff_tests.rs:parser_accepts_deferred_handoff_and_rejects_conflicting_dispositions
- Deferred Plan state, pending and recovery displays, source-input invalidation, local and shell preservation, mode changes, and destructive confirmations: codex-rs/tui/src/chatwidget/tests/plan_handoff_deferred.rs:deferred_plan_waits_for_next_prompt_in_default_mode,shell_command_does_not_consume_deferred_handoff,local_status_command_preserves_deferred_handoff,resume_picker_can_be_opened_without_discarding_deferred_handoff,mode_change_does_not_strand_the_next_deferred_prompt,restored_deferred_transfer_keeps_prompt_in_recovered_queue,accepted_source_input_invalidates_pending_handoff_in_live_and_replay,destructive_commands_confirm_before_discarding_deferred_plan,app_new_session_action_uses_the_same_discard_confirmation,reconnect_restores_pending_plan_without_automatic_transfer,deferred_handoff_waits_for_remote_workspace_image_preparation
- Rejected Default-mode settings clear pending handoff and restore the in-flight prompt: codex-rs/tui/src/chatwidget/tests/plan_handoff_deferred.rs:rejected_default_mode_update_cancels_deferred_plan
- Deferred fresh transfer, attachment offsets, failure recovery, App discard confirmation, and navigation replay: codex-rs/tui/src/app/tests/plan_handoff_deferred.rs:deferred_handoff_executes_plan_and_followup_in_fresh_thread,deferred_handoff_preserves_followup_image_and_text_element,deferred_start_failure_restores_prompt_and_pending_plan,new_session_event_confirms_before_discarding_deferred_plan,confirmed_inline_handoff_discards_pending_plan_and_preserves_arguments,prompt_edit_requires_confirmation_before_discarding_deferred_plan,deferred_plan_survives_thread_navigation_and_replay,rejected_deferred_execution_restores_combined_prompt_without_resend
- Automatic wrap-up completion passes the planning gate and renders both prompts: codex-rs/tui/src/chatwidget/tests/auto_handoff_tests.rs:automatic_wrap_up_completion_requests_planning_gate
- Automatic cancellation suppresses retry until a subsequent live below-threshold update rearms it: missing
- Automatic handoff cannot start with unknown context usage or at a blocked App gate: missing
- Attachment failure after leaving the source reports saved-source recovery instructions: missing
- TUI startup accepts threshold endpoints and rejects values outside the configured range: missing
- Opt-in automatic handoff ignores replayed usage, clears idle candidates when live context drops, and cancels wrap-up after compaction lowers use: codex-rs/tui/src/chatwidget/tests/auto_handoff_tests.rs:automatic_handoff_requires_opt_in_and_live_context_usage,automatic_handoff_candidate_clears_after_live_context_drop,compaction_below_threshold_cancels_automatic_wrap_up

### login-auth (auth and login behavior)

#### Description

Handoff does not alter authentication or credential selection.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Handoff does not change MCP transport, discovery, resources, or OAuth recovery.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Handoff uses existing model requests and Plan items without changing the lower-level API client.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Handoff is an interactive TUI feature and does not introduce non-interactive exec behavior.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Handoff introduces no dedicated telemetry export contract.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Remote workspace support reuses the existing execution boundary without adding service methods.

#### Status

Not covered

## Test Generation Notes

Use deterministic model responses containing completed Plan items and correlate source and
planning turn IDs. Include ordinary prose without a Plan, empty and oversized Plans, stale
transaction callbacks, same-turn steering, and source changes while destination creation is in
flight. Assert complete destination input and settings, retained source resumability, attachment
ranges, visible recovery state, and absence of automatic resubmission after rejection. For
automation, exercise threshold equality and adjacent values, unknown windows, replay-only usage,
primary-thread and descendant gates, wrap-up interruption, planning without a Plan, compaction
before versus during planning, and cancellation followed by live usage below and above threshold.
Keep component snapshots for discovery, validation errors, decisions, planning status, and recovery.
