# Workflows

## Summary

Workflows provide a user-facing mode for structured task execution. They combine workflow slash
commands, workflow-specific agent roles, and UI affordances that guide planning, implementation,
review, and repair without requiring users to manually assemble those steps.

## Behavior

Users can invoke workflow behavior from CLI commands and TUI slash commands. Workflow commands must
resolve to stable workflow definitions, apply the intended role/model/settings context, and preserve
normal Codex safety and approval behavior.

Workflow packages use one canonical TypeScript contract across CLI and hosted execution. A package
is a standalone git repository with v1 metadata, documentation, package metadata, source and tests,
and an ignored state directory. The shared workflow library owns discovery, input normalization,
scaffolding, validation, completion, and the Bun runner. Legacy packages remain discoverable so
users can locate them, but validation and execution return migration guidance instead of adapting
their older runtime contract.

Every invocation starts from an object supplied by `--input <JSON>` or bounded `--input @file`,
then applies explicit kebab-case flags (with repeats represented as arrays) and injects
`workingDirectory` only if it is absent. Positional `{argv,text}` input is not supported. Both
runtimes validate the input and output against explicit Draft 2020-12 schemas and invoke the same
`markdown.v1` formatter. The v1 top-level input object does not permit `maxProperties`, because
normalization may add `workingDirectory`. CLI progress is written to stderr; hosted execution additionally provides
the existing `requestUserInput` capability.

An executing workflow command participates in the normal turn lifecycle. Clients receive
`turn/started` before workflow output, and the TUI keeps the standard working and interrupt status
visible until the workflow completes, fails, or is canceled. Workflow results continue to render as
normal assistant messages.

Hosted `WorkflowCommandTask` executions expose `ctx.requestUserInput` so workflow code can
deterministically show user-interface questions and consume the structured response without asking
a model to interpret the answer. Version 1 supports single-choice questions and free-text entry.
Calls are serialized in invocation order, with at most one request on the wire at a time, and the
workflow turn remains active while it waits for an answer. Multi-select questions and
`autoResolutionMs` are not supported in this version.

Pending workflow input uses the normal app-server request-user-input path. A client that rejoins a
live thread receives the unresolved request again, while an answered request is not replayed.
Workflow child-process or app-server crash recovery is outside this contract; a terminated workflow
execution is not reconstructed from an interaction journal. Direct `codex workflow run` execution
is non-interactive and does not provide `ctx.requestUserInput`.

Workflow mode is visible in the TUI so users can tell when a workflow-oriented interaction is
active. Command autocomplete should surface workflow commands consistently with other slash
commands. Workflow command compatibility handling should preserve older command spellings where the
CLI intentionally supports them.

Workflow agent roles are built-in role definitions used by the workflow orchestration path. They
must remain discoverable, renderable in prompts, and compatible with the generic agent-role
application path.

## Entry Points

- [codex-rs/cli/src/workflow_cmd.rs](../cli/src/workflow_cmd.rs)
- [codex-rs/cli/src/workflow_cmd/compat.rs](../cli/src/workflow_cmd/compat.rs)
- [codex-rs/workflows/src/lib.rs](../workflows/src/lib.rs)
- [codex-rs/tui/src/workflow_commands.rs](../tui/src/workflow_commands.rs)
- [codex-rs/tui/src/slash_command.rs](../tui/src/slash_command.rs)
- [codex-rs/core/src/tasks/workflow_command.rs](../core/src/tasks/workflow_command.rs)
- [codex-rs/core/src/tasks/workflow_command/runtime.rs](../core/src/tasks/workflow_command/runtime.rs)
- [codex-rs/core/src/agent/role.rs](../core/src/agent/role.rs)
- [codex-rs/core/src/agent/builtins/workflow-coder.toml](../core/src/agent/builtins/workflow-coder.toml)
- [codex-rs/core/src/agent/builtins/workflow-code-reviewer.toml](../core/src/agent/builtins/workflow-code-reviewer.toml)

## Subfeatures

### Workflow Commands

#### Entry Points

- [codex-rs/cli/src/workflow_cmd.rs](../cli/src/workflow_cmd.rs)
- [codex-rs/cli/src/workflow_cmd/compat.rs](../cli/src/workflow_cmd/compat.rs)
- [codex-rs/tui/src/workflow_commands.rs](../tui/src/workflow_commands.rs)

#### Invariants

- CLI workflow commands keep their documented compatibility aliases.
- TUI slash command autocomplete lists workflow commands when workflow support is available.
- Static completion comes from top-level input-schema properties and enum/const values; an optional
  bounded dynamic hook augments those values only for the active workflow.
- Validation findings are deterministic, and successful validation prints exactly `valid`.
- Existing-ID and symlink collisions never overwrite or partially replace a workflow package.
- Workflow command dispatch should fail closed for unknown workflow names.

### Workflow Roles

#### Entry Points

- [codex-rs/core/src/agent/role.rs](../core/src/agent/role.rs)
- [codex-rs/core/src/agent/builtins/workflow-architect.toml](../core/src/agent/builtins/workflow-architect.toml)
- [codex-rs/core/src/agent/builtins/workflow-arch-reviewer.toml](../core/src/agent/builtins/workflow-arch-reviewer.toml)
- [codex-rs/core/src/agent/builtins/workflow-coder.toml](../core/src/agent/builtins/workflow-coder.toml)
- [codex-rs/core/src/agent/builtins/workflow-code-reviewer.toml](../core/src/agent/builtins/workflow-code-reviewer.toml)
- [codex-rs/core/src/agent/builtins/workflow-resilience-reviewer.toml](../core/src/agent/builtins/workflow-resilience-reviewer.toml)

#### Invariants

- Built-in workflow roles stay available through the normal role lookup path.
- Role-locked settings are surfaced to spawned agents and status surfaces.
- Workflow role prompts remain bounded and renderable as model context.

### Workflow Runtime User Input

#### Entry Points

- [codex-rs/core/src/tasks/workflow_command.rs](../core/src/tasks/workflow_command.rs)
- [codex-rs/core/src/tasks/workflow_command/runtime.rs](../core/src/tasks/workflow_command/runtime.rs)
- [codex-rs/core/src/tasks/workflow_command/runtime/host.rs](../core/src/tasks/workflow_command/runtime/host.rs)
- [codex-rs/workflows/src/interaction.rs](../workflows/src/interaction.rs)
- [sdk/typescript/src/workflowContext.ts](../../sdk/typescript/src/workflowContext.ts)
- [codex-rs/protocol/src/request_user_input.rs](../protocol/src/request_user_input.rs)
- [codex-rs/app-server/src/bespoke_event_handling.rs](../app-server/src/bespoke_event_handling.rs)
- [codex-rs/app-server/src/outgoing_message.rs](../app-server/src/outgoing_message.rs)
- [codex-rs/tui/src/bottom_pane/request_user_input/mod.rs](../tui/src/bottom_pane/request_user_input/mod.rs)

#### Invariants

- `ctx.requestUserInput` is available only to hosted workflow-command executions; standalone CLI
  workflow execution remains non-interactive and fails naturally if workflow code requires that
  host-provided function.
- Each call contains one to three questions with non-empty, unique question IDs. A question is
  either single-choice with unique option labels or free-text without options; multi-select is not
  accepted.
- Question IDs are snake_case and at most 64 characters. Headers are at most 12 characters,
  prompts 1,024 characters, and each question has at most 10 options whose labels are at most 80
  characters and descriptions at most 512 characters. An input-request control frame is at most 16
  KiB, a run may issue at most 64 requests, and a response frame is at most 4 MiB. Non-truncated
  final markdown is newline-terminated; all final markdown uses an 8 KiB persisted-output
  truncation limit.
- The runtime method accepts the existing `RequestUserInputArgs` JSON shape and resolves to the
  existing `RequestUserInputResponse` shape. The response maps every question ID to an `answers`
  array: a selected label comes first, followed by optional `user_note: <text>` input; a free-text
  answer contains only the note entry, and an unanswered question has an empty array. Missing
  answers, including the existing app-server fallback for a client or decoding failure, are
  canonicalized to empty arrays.
- TypeScript workflow authors can type the injected runtime with `WorkflowContext` exported by
  `@openai/codex-sdk`.
- `isOther: true` adds the client-provided `None of the above` choice, so authored options may not
  use that reserved label, and no authored option may begin with the reserved `user_note: ` prefix.
  Responses with unknown IDs, unavailable labels, or a shape inconsistent with the corresponding
  question are returned to workflow code as errors. A note-only Other response from a non-TUI
  client is canonicalized to the same label-plus-note representation.
- `isSecret: true` masks editor and history rendering in clients that support it. The plaintext
  answer still crosses app-server and is returned to workflow code, which remains responsible for
  not logging, formatting, or otherwise disclosing it.
- Calls are serviced in invocation order and only one request may be outstanding on the wire for a
  workflow turn. Concurrent calls do not overwrite, reorder, or cross-correlate answers. The
  app-server item ID combines the turn ID with the per-run request sequence so prompts cannot
  collide across workflow turns.
- Workflow interaction requests use the existing `item/tool/requestUserInput` request and
  `ToolRequestUserInputResponse` response shapes. A selected option is returned by label, and typed
  text uses the existing `user_note: ...` answer entry.
- Version 1 does not accept `autoResolutionMs`; every workflow interaction waits for client
  resolution or interruption.
- Host control events use a run-private file channel, and response frames use child stdin; both
  stay separate from workflow stdout/stderr and are never automatically rendered or persisted.
  Only markdown deliberately returned by the workflow formatter becomes final workflow output.
- While a request is pending, the workflow turn stays in progress and does not emit a terminal
  lifecycle event. Interrupting the turn closes the pending request, terminates the workflow child,
  and does not record partial workflow output. Termination includes workflow descendants on Unix
  and Windows.
- Rejoining a live thread replays the same unresolved app-server request with the same request ID
  and payload. Resolved requests are not replayed and neither workflow-child nor app-server restart
  recovery is supported.
- Malformed, oversized, duplicate-ID, unknown-version, and unsupported interaction requests fail
  with a bounded actionable error instead of hanging or guessing.

## Invariants

- Workflow behavior remains opt-in through explicit workflow commands or mode selection.
- Workflow commands do not bypass normal approval, sandbox, or permission behavior.
- Workflow UI state reflects the current workflow mode without changing the underlying turn model.
- Accepted workflow commands emit one start lifecycle event and one terminal lifecycle event;
  workflow execution must not appear idle while its task is active.
- Workflow input requests and answers are correlated and delivered directly to workflow code
  without model mediation, with at most one unresolved request per workflow turn.
- Pending workflow input is replayable while its thread and workflow process remain live, and every
  response resolves its request at most once.
- Built-in workflow roles are treated as part of the role catalog, not special-cased prompt text.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise workflow role application through the normal agent-role path,
workflow role prompts entering bounded model context, and a complete
planning-implementation-review workflow execution path.

#### Test cases

- Each built-in workflow role is discoverable and applied: codex-rs/core/tests/suite/workflows__agent_roles.rs:built_in_workflow_roles_are_discoverable_from_spawn_agent_tool,workflow_spawn_path_applies_planning_implementation_review_and_repair_roles
- Full workflow execution covers planning, implementation, review, and repair roles: codex-rs/core/tests/suite/workflows__agent_roles.rs:workflow_spawn_path_applies_planning_implementation_review_and_repair_roles

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise workflow command RPC execution, start and completion lifecycle
notifications, persisted workflow output, next-turn context after workflow output is recorded, and
the hosted workflow user-input round trip. It should also verify unresolved-request replay on live
thread resume, absence of replay after resolution, and that interruption resolves a pending input
request and terminates the workflow without partial output.

#### Test cases

- Workflow command RPC records assistant output and next-turn context: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_records_assistant_output_and_next_turn_context
- A fresh canonical scaffold runs through the real hosted Bun path unchanged: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_runs_fresh_scaffold_with_real_bun
- Hosted schema, malformed-export, and legacy migration failures produce failed turns without formatted output: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_reports_canonical_and_legacy_contract_failures
- Workflow command RPC rejects execution during an active turn: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_rejects_active_turn
- Hosted workflow commands receive correlated single-choice and free-text answers across sequential requests, replay pending input on live resume, and do not replay resolved input: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_round_trips_choice_and_freeform_user_input
- Interrupting a workflow clears its pending input request and produces no partial result: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_interrupt_clears_pending_user_input

### cli (main CLI command behavior)

#### Description

CLI coverage should exercise workflow command parsing, compatibility aliases, and failure behavior
for unknown workflows.

#### Test cases

- Canonical execution and migration diagnostics are covered: codex-rs/cli/tests/workflows__cli.rs:workflow_alias_positional_args_report_migration_guidance,workflow_run_executes_fresh_scaffold_and_formats_markdown,workflow_run_legacy_package_reports_migration_guidance,workflow_run_rejects_an_incomplete_canonical_package,workflow_run_by_nested_id_merges_json_input_and_flags,workflow_run_invokes_bun_with_structured_input
- Safe scaffolding and exact validation output are covered: codex-rs/cli/tests/workflows__cli.rs:workflow_develop_refuses_to_overwrite_an_existing_target,workflow_develop_refuses_to_traverse_a_symlink,workflow_develop_scaffolds_project_workflow,workflow_validate_prints_exact_success_marker_for_fresh_scaffold,workflow_validate_reports_an_undiscoverable_package_by_safe_id_path,workflow_validate_reports_invalid_workflow_at_cli_boundary
- Discovery and compatibility management behavior remains covered: codex-rs/cli/tests/workflows__cli.rs:exact_workflow_id_takes_precedence_over_another_packages_alias,workflow_list_outputs_discovered_commands_as_json,workflow_list_requires_workflows_feature,workflow_management_commands_match_old_surface,workflow_recover_uses_the_same_canonical_input_normalization_as_run,workflow_run_unknown_command_reports_available_commands,workflow_show_json_and_root_status_cover_management_outputs
- Compatibility editing and repair behavior remains covered: codex-rs/cli/tests/workflows__cli.rs:workflow_alias_invokes_bun_like_old_cli_surface,workflow_editing_commands_match_old_surface,workflow_fix_keeps_valid_code_review_workflow_without_usage_options,workflow_fix_rejects_runtime_arguments,workflow_fix_repairs_workflow_without_running_unsupported_fix_action,workflow_fix_scaffolds_missing_workflow_source_for_discovery_fallback,workflow_fix_tolerates_broken_metadata_and_source_without_running_workflow,workflow_repair_alias_repairs_workflow_without_running_workflow_runtime

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise workflow slash autocomplete, workflow option completion, workflow
command insertion, and the standard running indicator in a live terminal session. It should also
exercise visible workflow mode state during a submitted mocked turn.

#### Test cases

- Workflow slash autocomplete is covered: codex-rs/tui/tests/suite/workflows__slash_autocomplete.rs:workflow_command_autocompletes_in_live_tui
- Workflow command running status is covered: codex-rs/tui/tests/suite/workflows__slash_autocomplete.rs:workflow_command_shows_running_status_in_live_tui
- Workflow mode footer and mocked turn submission are covered: codex-rs/tui/tests/suite/workflows__mode.rs:workflow_slash_enters_mode_and_submits_mocked_ai_turn

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise workflow mode indicators, workflow command option rendering,
the standard task status shown while a workflow command is running, and the shared user-input
overlay used by hosted workflow questions. Pending prompts should remain visible across live thread
switches and disappear after they are answered or resolved.

#### Test cases

- Workflow mode indicators, running status, and slash-command dispatch are covered: codex-rs/tui/src/chatwidget/tests/workflows__slash_commands.rs:bare_workflow_command_dispatches_structured_workflow_op,bare_workflow_slash_enters_workflow_mode,bare_workflow_slash_reports_disabled_when_feature_off,queued_malformed_workflow_command_reports_error_and_drains_next_input,queued_workflow_command_dispatches_after_active_turn,running_workflow_command_uses_standard_task_status_snapshot,workflow_command_appears_in_slash_popup_when_enabled,workflow_command_is_hidden_and_rejected_when_feature_disabled,workflow_command_rejects_malformed_args_without_clearing_draft,workflow_command_with_args_dispatches_structured_input_json,workflow_done_slash_exits_to_default_mode,workflow_slash_with_args_dispatches_workflow_cli_command
- TUI schema/hook request shaping is covered: codex-rs/tui/src/bottom_pane/chat_composer/workflow_completion_tests.rs:completion_request_tracks_partial_input_and_active_value,field_and_value_results_become_popup_hints
- Workflow completion popup rendering is covered: codex-rs/tui/src/bottom_pane/command_popup.rs:workflow_exact_command_shows_schema_field_hints,workflow_option_value_completion_uses_value_hint_enums
- Pending user-input prompts replay while unresolved and are removed after an answer: codex-rs/tui/src/app/pending_interactive_replay.rs:thread_event_snapshot_keeps_pending_request_user_input,thread_event_snapshot_drops_resolved_request_user_input_after_user_answer,thread_event_snapshot_keeps_newer_request_user_input_pending_when_same_turn_has_queue

### login-auth (auth and login behavior)

#### Description

Workflows do not define login, logout, token refresh, credential selection, or cached auth
behavior.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Workflows are not exposed as Codex-as-MCP-server tools.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Workflows do not change MCP client transport, startup, resource, OAuth, or recovery behavior.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Workflows do not change lower-level Codex API client or protocol behavior.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Workflow commands are covered by the main CLI workflow surface and do not change non-interactive
exec mode semantics.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Workflows do not currently define telemetry, metric, or export contract changes.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Workflows do not change exec-server process, filesystem, HTTP, relay, or WebSocket behavior.

#### Status

Not covered

## Test Generation Notes

Generate tests that cover canonical package loading, bounded input normalization, safe scaffolding,
deterministic validation, schema-plus-hook completion, compatibility aliases and migration errors,
slash autocomplete, unknown workflow handling, workflow mode indicators, and each built-in
workflow role being discoverable and applied through the normal agent-role path.

The shared `codex-workflows` crate keeps focused unit and real-Bun runtime coverage next to its
discovery, input, scaffold, validation, completion, and runner modules; those library tests do not
map to a client-facing test place above.

Runtime unit coverage for `WorkflowCommandTask` should exercise framed request parsing separately
from workflow stdout, valid single-choice and free-text requests, response canonicalization, and
exact question/answer correlation. Negative cases should cover duplicate IDs and labels, invalid
question and option bounds, the reserved Other label, `autoResolutionMs`, multi-select requests,
unknown protocol versions or methods, malformed frames, and oversized frames. App-server coverage
should exercise the complete response round trip and cancellation while input is pending.
Run `just workflow-runtime-test` to exercise the canonical module/schema checks, shared
`markdown.v1` formatter, completion bounds, embedded JavaScript queue, input-request size gate,
private control channel, completion handshake, Unicode truncation, and trailing-newline behavior
directly. The Rust CI workflow installs a pinned Bun release and runs this test. Windows unit-test
coverage additionally verifies that closing the workflow Job Object terminates descendants.
