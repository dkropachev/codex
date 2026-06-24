# Skills

## Summary

Skills provide reusable, named instructions and assets that Codex can load into a turn when a user
explicitly invokes a skill or when configured skill rules select one. Skills are user-facing because
they alter model behavior, appear in TUI controls, and can be enabled or disabled before their
instructions are injected.

## Behavior

Skills are discovered from system, user, plugin, and remote skill sources. Each skill has a stable
name, instructions, optional assets, and optional invocation metadata. Skill loading must respect
configuration rules and source precedence.

When skills are enabled for a turn, Codex injects bounded skill instructions into model-visible
context. Skill instructions must preserve the selected skill content without rewriting previous
conversation history. Disabled skills must not be injected as active instructions.

Skill lists and toggles let clients expose available skills without starting a model turn. TUI skill
controls should reflect availability and selection state consistently with app-server responses.

## Entry Points

- [codex-rs/core-skills/src/manager.rs](../core-skills/src/manager.rs)
- [codex-rs/core-skills/src/loader.rs](../core-skills/src/loader.rs)
- [codex-rs/core-skills/src/injection.rs](../core-skills/src/injection.rs)
- [codex-rs/core/src/skills.rs](../core/src/skills.rs)
- [codex-rs/core/src/context/available_skills_instructions.rs](../core/src/context/available_skills_instructions.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/plugin.rs](../app-server-protocol/src/protocol/v2/plugin.rs)
- [codex-rs/app-server/src/request_processors/catalog_processor.rs](../app-server/src/request_processors/catalog_processor.rs)
- [codex-rs/app-server/src/skills_watcher.rs](../app-server/src/skills_watcher.rs)
- [codex-rs/tui/src/chatwidget/skills.rs](../tui/src/chatwidget/skills.rs)
- [codex-rs/tui/src/bottom_pane/skill_popup.rs](../tui/src/bottom_pane/skill_popup.rs)
- [codex-rs/tui/src/bottom_pane/skills_toggle_view.rs](../tui/src/bottom_pane/skills_toggle_view.rs)

## Subfeatures

### Skill Discovery

#### Entry Points

- [codex-rs/core-skills/src/loader.rs](../core-skills/src/loader.rs)
- [codex-rs/core-skills/src/manager.rs](../core-skills/src/manager.rs)
- [codex-rs/core-skills/src/config_rules.rs](../core-skills/src/config_rules.rs)
- [codex-rs/app-server/src/request_processors/catalog_processor.rs](../app-server/src/request_processors/catalog_processor.rs)
- [codex-rs/app-server/src/skills_watcher.rs](../app-server/src/skills_watcher.rs)

#### Invariants

- Skill names are stable across discovery and rendering.
- Invalid skill definitions are reported without hiding valid skills.
- Skill list APIs can report available skills before a model turn starts.

### Skill Injection

#### Entry Points

- [codex-rs/core-skills/src/injection.rs](../core-skills/src/injection.rs)
- [codex-rs/core-skills/src/render.rs](../core-skills/src/render.rs)
- [codex-rs/core/src/skills.rs](../core/src/skills.rs)
- [codex-rs/core/src/context/available_skills_instructions.rs](../core/src/context/available_skills_instructions.rs)

#### Invariants

- Injected skill instructions are bounded.
- Skill instructions are added incrementally without rewriting prior model-visible history.
- Disabled skills and skills awaiting approval are not injected as active skills.

### Skill UI

#### Entry Points

- [codex-rs/tui/src/chatwidget/skills.rs](../tui/src/chatwidget/skills.rs)
- [codex-rs/tui/src/bottom_pane/skill_popup.rs](../tui/src/bottom_pane/skill_popup.rs)
- [codex-rs/tui/src/bottom_pane/skills_toggle_view.rs](../tui/src/bottom_pane/skills_toggle_view.rs)
- [codex-rs/tui/src/skills_helpers.rs](../tui/src/skills_helpers.rs)

#### Invariants

- TUI controls distinguish available, enabled, and disabled skills.
- Skill toggles update client-visible state without losing the current composer input.
- Skill popups render long lists without dropping selectable skills.

## Invariants

- Skills alter model-visible behavior only through explicit skill injection paths.
- Skill discovery and skill injection are separate so clients can list skills without starting a
  turn.
- Disabled skill state gates active instruction injection.
- Skill context remains bounded and source-aware.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise skill discovery, explicit invocation, disabled
skill exclusion, and bounded model-context injection during agent turns.

#### Test cases

- Skill model-context behavior is covered: codex-rs/core/tests/suite/skills__agent_context.rs:user_turn_includes_skill_instructions
- Skill script sandbox behavior is covered: codex-rs/core/tests/suite/skills__approval.rs:shell_zsh_fork_skill_scripts_ignore_declared_permissions,shell_zsh_fork_still_enforces_workspace_write_sandbox

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise skill listing APIs before a model turn starts.

#### Test cases

- Skill list API behavior is covered: codex-rs/app-server/tests/suite/v2/skills__list.rs:skills_changed_notification_is_emitted_after_skill_change,skills_extra_roots_set_updates_process_runtime_roots,skills_list_accepts_relative_cwds,skills_list_excludes_plugin_skills_when_workspace_codex_plugins_disabled,skills_list_loads_remote_installed_plugin_skills_from_cache,skills_list_preserves_requested_cwd_order,skills_list_skips_cwd_roots_when_environment_disabled,skills_list_uses_cached_result_until_force_reload

### cli (main CLI command behavior)

#### Description

Skills currently have app-server, TUI, plugin, and agent surfaces but no dedicated CLI command
contract.

#### Status

Not covered

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise live skill selection, enabled and disabled state, toggling, and
composer preservation.

#### Test cases

- Live TUI skill selection and submission are covered: codex-rs/tui/tests/suite/skills__live.rs:skill_selection_submits_selected_skill
- Disabled skill state, toggling, and composer preservation are covered: codex-rs/tui/tests/suite/skills__live.rs:skill_toggle_enables_disabled_skill_and_preserves_draft

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise skill popup rendering, long-list behavior, selection state,
disabled state, and toggle view behavior.

#### Test cases

- Skill popup rendering and long-list behavior are covered: codex-rs/tui/src/bottom_pane/skills__skill_popup.rs:filtered_mentions_preserve_results_beyond_popup_height,scrolling_mentions_shifts_rendered_window_snapshot,display_name_match_sorting_beats_worse_secondary_search_term_matches,query_match_score_sorts_before_plugin_rank_bias
- Skill selection, disabled, and toggle states are covered: codex-rs/tui/src/bottom_pane/skills__toggle_view.rs:renders_basic_popup,footer_hint_uses_list_keymap_accept_and_cancel,space_toggles_selected_skill_and_emits_event

### login-auth (auth and login behavior)

#### Description

Skills do not change login, logout, token refresh, credential selection, or cached auth behavior.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Skills are not exposed as Codex-as-MCP-server tools.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Skills do not change MCP client transport, startup, resource, OAuth, or recovery behavior.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Skills do not change lower-level Codex API client or protocol behavior.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Skills do not change non-interactive exec mode command behavior.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Skills do not currently define telemetry, metric, or export contract changes.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Skills do not change exec-server process, filesystem, HTTP, relay, or WebSocket behavior.

#### Status

Not covered

## Test Generation Notes

Generate tests for discovery from each supported source, invalid skill definitions, list APIs,
disabled skills, bounded instruction injection, explicit invocation, and TUI skill selection state.
