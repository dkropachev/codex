# Clarification Questions

## Summary

The TUI retains unanswered asynchronous clarification questions after a successful
assistant turn, so users can read and answer them without racing the model.
This is a fork-specific change to the upstream turn-end lifecycle.

## Behavior

- During a running turn, new questions open when the main composer is empty and
  no other popup owns input. An existing main draft keeps its focus; the question
  counter and configured answer shortcut remain available.
- Successful turn completion preserves unanswered questions, the current question,
  selected choices, typed drafts, and whether the editor is expanded or collapsed.
  Completion alone neither submits an answer nor keeps the assistant running.
- The same behavior applies to questions reconstructed from a completed turn's
  final message when item notifications were missing.
- Users can reopen and submit retained questions after completion. Submission
  uses the existing structured user-message path and removes only that question.
  Other pending questions and the main composer draft remain intact.
- In-process thread navigation preserves the editor state. Historical replay
  alone does not reopen questions after a TUI restart.
- Explicitly skipping a question, a verified answer from another client, or an
  accepted ordinary model prompt retires the corresponding pending questions
  through the existing paths. Local commands do not silently answer questions.
- Interrupted and failed turns retain the existing recovery behavior: pending
  questions close and nonblank typed answers return to the main composer unsent.
  Existing policy-violation handling can still clear drafts.
- A plan implementation prompt must not cover unanswered clarification questions.
- The blocking `request_user_input` overlay, tool results, model instructions,
  configuration, and app-server wire formats retain their existing contracts.

## Entry Points

- [Turn notifications](../tui/src/chatwidget/protocol.rs)
- [Question input delivery](../tui/src/chatwidget/questions.rs)
- [Retained question state](../tui/src/bottom_pane/async_questions/state.rs)

## Subfeatures

### Question retention

#### Entry Points

- [Turn notifications](../tui/src/chatwidget/protocol.rs)
- [Question input delivery](../tui/src/chatwidget/questions.rs)

#### Invariants

- Successful completion preserves the local editor and pending answers.
- Submission, skip, external-answer, and superseding-prompt paths retire questions.

### Aborted-turn recovery

#### Entry Points

- [Draft recovery](../tui/src/bottom_pane/questions.rs)

#### Invariants

- Interrupted and failed turns recover nonblank typed drafts into the main composer.

## Invariants

- Completion never sends an answer or treats a highlighted option as approval.
- An accepted answer submission removes only that question; other answers remain.
- Main composer drafts and question drafts remain separate after success.
- Retention is local UI state and does not rewrite model history or change tool results.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

This change controls client editor lifetime; agent tool execution and model-visible
tool results do not change.

#### Status

Not covered

### app-server-api (app-server API behavior)

#### Description

Existing requests and notifications carry the question data without a wire change.

#### Status

Not covered

### cli (main CLI command behavior)

#### Description

No top-level CLI command or argument changes.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

The editor lifecycle is exercised through focused TUI notification and input tests;
no additional terminal-process behavior is introduced.

#### Status

Not covered

### tui-component (focused TUI component behavior)

#### Description

Question lifetime, answer delivery, draft recovery, notifications, and plan prompts.

#### Test cases

- Completed turns retain multiple drafts across state restoration and allow later submission: codex-rs/tui/src/chatwidget/tests/question_turn_end_tests.rs:completed_turn_retains_question_drafts_until_submission
- Collapsed choices remain discoverable and answerable without replacing the main draft: codex-rs/tui/src/chatwidget/tests/question_turn_end_tests.rs:completed_turn_retains_collapsed_choices_and_main_draft
- Question notifications survive success and expire after interruption or failure: codex-rs/tui/src/chatwidget/tests/question_notifications_tests.rs:async_question_notifications_follow_question_lifetime
- Missing item notifications recover a pending question from the final turn payload: codex-rs/tui/src/chatwidget/tests/app_server.rs:live_app_server_turn_completion_repairs_dropped_message_deltas
- Interrupted and failed turns recover collapsed drafts: codex-rs/tui/src/chatwidget/tests/question_turn_end_tests.rs:question_turn_end_recovers_collapsed_drafts_on_interruption_and_failure
- Pending clarifications take priority over plan implementation prompts: codex-rs/tui/src/chatwidget/tests/plan_mode.rs:plan_implementation_popup_skips_unanswered_questions

### login-auth (auth and login behavior)

#### Description

No authentication behavior changes.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

No MCP transport or resource behavior changes.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

No model API request or response handling changes.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Noninteractive execution has no question editor.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

No telemetry or export changes.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

No execution-service boundary changes.

#### Status

Not covered

## Test Generation Notes

Exercise open and collapsed questions, multiple drafts, selected choices, repeated
completion notifications, in-process state restoration, and submission after the
model stops. Keep interruption/failure recovery coverage and snapshots. Check the
final-turn fallback when item notifications are missing, and ensure plan prompts
do not hide pending clarification questions. Do not auto-submit any retained draft.
