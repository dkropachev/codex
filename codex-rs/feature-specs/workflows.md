# Workflows

## Summary

Workflows provide a user-facing mode for structured task execution. They combine workflow slash
commands, workflow-specific agent roles, and UI affordances that guide planning, implementation,
review, and repair without requiring users to manually assemble those steps.

## Behavior

Users can invoke workflow behavior from CLI commands and TUI slash commands. Workflow commands must
resolve to stable workflow definitions, apply the intended role/model/settings context, and preserve
normal Codex safety and approval behavior.

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
- [codex-rs/tui/src/workflow_commands.rs](../tui/src/workflow_commands.rs)
- [codex-rs/tui/src/slash_command.rs](../tui/src/slash_command.rs)
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

## Invariants

- Workflow behavior remains opt-in through explicit workflow commands or mode selection.
- Workflow commands do not bypass normal approval, sandbox, or permission behavior.
- Workflow UI state reflects the current workflow mode without changing the underlying turn model.
- Built-in workflow roles are treated as part of the role catalog, not special-cased prompt text.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise workflow role application through the normal agent-role path,
workflow role prompts entering bounded model context, and a complete
planning-implementation-review workflow execution path.

#### Test cases

- Each built-in workflow role is discoverable and applied: missing
- Full workflow execution covers planning, implementation, review, and repair roles: missing

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise workflow command RPC execution, persisted workflow output,
notifications, and next-turn context after workflow output is recorded.

#### Test cases

- Workflow command RPC records assistant output and next-turn context: codex-rs/app-server/tests/suite/v2/workflows__thread_command.rs:thread_workflow_command_records_assistant_output_and_next_turn_context

### cli (main CLI command behavior)

#### Description

CLI coverage should exercise workflow command parsing, compatibility aliases, and failure behavior
for unknown workflows.

#### Test cases

- Workflow CLI behavior is covered part 1: codex-rs/cli/tests/workflows__cli.rs:workflow_alias_invokes_bun_like_old_cli_surface,workflow_alias_positional_args_use_legacy_payload,workflow_develop_scaffolds_project_workflow,workflow_editing_commands_match_old_surface,workflow_fix_rejects_runtime_arguments,workflow_fix_repairs_workflow_without_running_unsupported_fix_action,workflow_fix_scaffolds_missing_workflow_source_for_discovery_fallback,workflow_fix_tolerates_broken_metadata_and_source_without_running_workflow
- Workflow CLI behavior is covered part 2: codex-rs/cli/tests/workflows__cli.rs:workflow_list_outputs_discovered_commands_as_json,workflow_list_requires_workflows_feature,workflow_management_commands_match_old_surface,workflow_recover_invokes_bun_with_resume_action,workflow_repair_alias_repairs_workflow_without_running_workflow_runtime,workflow_run_by_nested_id_merges_json_input_and_flags,workflow_run_invokes_bun_with_structured_input,workflow_run_unknown_command_reports_available_commands
- Workflow CLI behavior is covered part 3: codex-rs/cli/tests/workflows__cli.rs:workflow_show_json_and_root_status_cover_management_outputs,workflow_validate_reports_invalid_workflow_at_cli_boundary

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise workflow slash autocomplete, workflow option completion, and
workflow command insertion in a live terminal session. It should also exercise visible workflow mode
state during a submitted mocked turn.

#### Test cases

- Workflow slash autocomplete is covered: codex-rs/tui/tests/suite/workflows__slash_autocomplete.rs:workflow_command_autocompletes_in_live_tui
- Workflow mode footer and mocked turn submission are covered: codex-rs/tui/tests/suite/workflows__mode.rs:workflow_slash_enters_mode_and_submits_mocked_ai_turn

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise workflow mode indicators and workflow command option
rendering.

#### Test cases

- Workflow mode indicators are covered: missing
- Workflow command discovery and option handling are covered part 1: codex-rs/tui/src/workflows__commands_tests.rs:builds_shell_command_for_workflow_directory,discovers_home_and_project_workflow_commands,discovers_nested_workflow_ids,discovers_workflow_usage_option_hints,ignores_missing_or_invalid_command_names,merges_input_object_and_flags_without_overriding_working_directory,parses_workflow_args_into_input_json,project_workflow_overrides_home_command_name
- Workflow command discovery and option handling are covered part 2: codex-rs/tui/src/workflows__commands_tests.rs:rejects_malformed_workflow_args

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

Generate tests that cover workflow command parsing, compatibility aliases, slash autocomplete,
unknown workflow handling, workflow mode indicators, and each built-in workflow role being
discoverable and applied through the normal agent-role path.
