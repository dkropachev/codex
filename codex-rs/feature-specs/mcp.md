# MCP

## Summary

MCP lets Codex connect to external Model Context Protocol servers, expose their tools and resources,
handle server elicitation, and run Codex itself as an MCP server. It is user-facing because MCP
servers affect available tools, approval prompts, model-visible tool definitions, and client status
surfaces.

## Behavior

Configured MCP servers are started, monitored, and exposed to model turns according to their
configuration and runtime status. MCP tools and resources must be discoverable, bounded, and routed
through the same approval and sandbox-aware tool machinery as built-in tools.

MCP server status is visible through app-server and TUI surfaces. Server failures should be reported
without dropping unrelated servers. MCP elicitation lets a server request structured user input, and
clients must preserve the request schema and user response semantics.

The Codex MCP server exposes Codex operations to external MCP clients. It must preserve approval
behavior for exec and patch requests and return tool results in the MCP protocol shape expected by
clients.

## Entry Points

- [codex-rs/codex-mcp/src/connection_manager.rs](../codex-mcp/src/connection_manager.rs)
- [codex-rs/codex-mcp/src/runtime.rs](../codex-mcp/src/runtime.rs)
- [codex-rs/core/src/mcp.rs](../core/src/mcp.rs)
- [codex-rs/core/src/session/mcp.rs](../core/src/session/mcp.rs)
- [codex-rs/core/src/mcp_tool_call.rs](../core/src/mcp_tool_call.rs)
- [codex-rs/core/src/mcp_tool_exposure.rs](../core/src/mcp_tool_exposure.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/mcp.rs](../app-server-protocol/src/protocol/v2/mcp.rs)
- [codex-rs/app-server/src/request_processors/mcp_processor.rs](../app-server/src/request_processors/mcp_processor.rs)
- [codex-rs/cli/src/mcp_cmd.rs](../cli/src/mcp_cmd.rs)
- [codex-rs/mcp-server/src/lib.rs](../mcp-server/src/lib.rs)

## Subfeatures

### MCP Server Connections

#### Entry Points

- [codex-rs/codex-mcp/src/connection_manager.rs](../codex-mcp/src/connection_manager.rs)
- [codex-rs/codex-mcp/src/runtime.rs](../codex-mcp/src/runtime.rs)
- [codex-rs/core/src/session/mcp.rs](../core/src/session/mcp.rs)
- [codex-rs/app-server/src/mcp_refresh.rs](../app-server/src/mcp_refresh.rs)

#### Invariants

- One failing server does not hide status for unrelated servers.
- Server status refreshes do not mutate tool-call history.
- Server startup errors are visible to clients.

### MCP Tools And Resources

#### Entry Points

- [codex-rs/core/src/mcp_tool_call.rs](../core/src/mcp_tool_call.rs)
- [codex-rs/core/src/mcp_tool_exposure.rs](../core/src/mcp_tool_exposure.rs)
- [codex-rs/core/src/tools/handlers/mcp.rs](../core/src/tools/handlers/mcp.rs)
- [codex-rs/core/src/tools/handlers/mcp_resource.rs](../core/src/tools/handlers/mcp_resource.rs)
- [codex-rs/app-server/src/request_processors/mcp_processor.rs](../app-server/src/request_processors/mcp_processor.rs)

#### Invariants

- MCP tool definitions are bounded before they enter model-visible context.
- MCP tool calls preserve server/tool identity through approval, execution, and result reporting.
- MCP resources and resource templates are listed and read through explicit resource APIs.

### MCP Elicitation

#### Entry Points

- [codex-rs/codex-mcp/src/elicitation.rs](../codex-mcp/src/elicitation.rs)
- [codex-rs/codex-mcp/src/auth_elicitation.rs](../codex-mcp/src/auth_elicitation.rs)
- [codex-rs/app-server/src/request_processors/mcp_processor.rs](../app-server/src/request_processors/mcp_processor.rs)
- [codex-rs/tui/src/bottom_pane/mcp_server_elicitation.rs](../tui/src/bottom_pane/mcp_server_elicitation.rs)

#### Invariants

- Elicitation requests preserve the server-provided schema.
- User responses are associated with the elicitation request they answer.
- Authentication elicitation does not expose sensitive values in status displays.

### Codex MCP Server

#### Entry Points

- [codex-rs/mcp-server/src/lib.rs](../mcp-server/src/lib.rs)
- [codex-rs/mcp-server/src/message_processor.rs](../mcp-server/src/message_processor.rs)
- [codex-rs/mcp-server/src/codex_tool_runner.rs](../mcp-server/src/codex_tool_runner.rs)
- [codex-rs/mcp-server/src/exec_approval.rs](../mcp-server/src/exec_approval.rs)
- [codex-rs/mcp-server/src/patch_approval.rs](../mcp-server/src/patch_approval.rs)

#### Invariants

- External MCP clients receive stable tool schemas for Codex operations.
- Exec and patch operations preserve approval behavior.
- Tool results are serialized in MCP-compatible response shapes.

## Invariants

- MCP server failures are isolated by server.
- MCP tools/resources enter model context only through bounded exposure paths.
- MCP tool calls preserve server and tool identity throughout execution.
- Elicitation requests and responses are correlated explicitly.
- Codex MCP server operations preserve normal Codex approval behavior.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise MCP tool metadata, tool exposure, resource and file behavior, hook
interaction, and model-turn routing through MCP clients.

#### Test cases

- MCP client tool-call behavior is covered part 1: codex-rs/core/tests/suite/mcp__client_tool_calls.rs:local_stdio_server_uses_runtime_fallback_cwd_when_config_omits_cwd,remote_stdio_env_var_source_does_not_copy_local_env,stdio_image_responses_are_sanitized_for_text_only_model,stdio_image_responses_preserve_original_detail_metadata,stdio_image_responses_round_trip,stdio_mcp_parallel_tool_calls_default_false_runs_serially,stdio_mcp_parallel_tool_calls_opt_in_runs_concurrently,stdio_mcp_read_only_tool_calls_run_concurrently_without_server_opt_in
- MCP client tool-call behavior is covered part 2: codex-rs/core/tests/suite/mcp__client_tool_calls.rs:stdio_mcp_tool_call_includes_sandbox_state_meta,stdio_server_propagates_explicit_local_env_var_source,stdio_server_propagates_whitelisted_env_vars,stdio_server_round_trip,stdio_server_uses_configured_cwd_before_runtime_fallback,streamable_http_tool_call_round_trip,streamable_http_with_oauth_round_trip
- MCP hook interaction is covered: codex-rs/core/tests/suite/mcp__hooks.rs:post_tool_use_records_mcp_tool_payload_and_context_with_legacy_prefixed_names,post_tool_use_records_mcp_tool_payload_and_context_with_non_prefixed_names,pre_tool_use_blocks_mcp_tool_before_execution_with_legacy_prefixed_names,pre_tool_use_blocks_mcp_tool_before_execution_with_non_prefixed_names,pre_tool_use_rewrites_mcp_tool_before_execution
- OpenAI file MCP behavior is covered: codex-rs/core/tests/suite/mcp__openai_file.rs:codex_apps_file_params_upload_local_paths_before_mcp_tool_call
- MCP turn metadata is covered: codex-rs/core/tests/suite/mcp__turn_metadata.rs:approved_mcp_tool_call_metadata_records_prior_user_input_request,mcp_tool_call_metadata_records_prior_request_user_input_tool

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise MCP tool APIs, resource APIs, server status, and elicitation
request/response behavior.

#### Test cases

- MCP resource API behavior is covered: codex-rs/app-server/tests/suite/v2/mcp__resource.rs:mcp_resource_read_returns_error_for_unknown_thread,mcp_resource_read_returns_resource_contents,mcp_resource_read_returns_resource_contents_without_thread
- MCP elicitation API behavior is covered: codex-rs/app-server/tests/suite/v2/mcp__server_elicitation.rs:mcp_server_elicitation_round_trip
- MCP server status API behavior is covered: codex-rs/app-server/tests/suite/v2/mcp__server_status.rs:mcp_server_status_list_keeps_tools_for_sanitized_name_collisions,mcp_server_status_list_returns_raw_server_and_tool_names,mcp_server_status_list_tools_and_auth_only_skips_slow_inventory_calls,mcp_server_status_list_uses_thread_project_local_config
- MCP tool API behavior is covered: codex-rs/app-server/tests/suite/v2/mcp__tool.rs:mcp_server_tool_call_forwards_url_elicitation,mcp_server_tool_call_returns_error_for_unknown_thread,mcp_server_tool_call_returns_tool_result,mcp_server_tool_call_round_trips_elicitation,mcp_tool_call_completion_notification_contains_truncated_large_result

### cli (main CLI command behavior)

#### Description

CLI coverage should exercise MCP server configuration commands.

#### Test cases

- MCP add/remove CLI behavior is covered: codex-rs/cli/tests/mcp__add_remove.rs:add_and_remove_server_updates_global_config,add_cant_add_command_and_url,add_streamable_http_rejects_removed_flag,add_streamable_http_with_custom_env_var,add_streamable_http_with_oauth_options,add_streamable_http_without_manual_token,add_with_env_preserves_key_order_and_values,profile_mcp_reports_legacy_profile_migration
- MCP list/get CLI behavior is covered: codex-rs/cli/tests/mcp__list.rs:get_disabled_server_shows_single_line,list_and_get_render_expected_output,list_shows_empty_state

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise MCP startup-warning interaction and elicitation form submission
flows in a live terminal session.

#### Test cases

- MCP startup-warning interaction works in live TUI: missing
- MCP elicitation form submission works in live TUI: missing

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise MCP startup warning rendering, startup warning interaction
state, and elicitation form rendering and response state.

#### Test cases

- MCP startup warning rendering and interaction state are covered part 1: codex-rs/tui/src/chatwidget/tests/mcp__startup.rs:app_server_mcp_startup_after_lag_can_settle_without_starting_updates,app_server_mcp_startup_after_lag_includes_runtime_servers_with_expected_set,app_server_mcp_startup_after_lag_preserves_partial_terminal_only_round,app_server_mcp_startup_failure_renders_warning_history,app_server_mcp_startup_lag_settles_startup_and_ignores_late_updates,app_server_mcp_startup_next_round_after_lag_can_settle_without_starting_updates,app_server_mcp_startup_next_round_discards_stale_terminal_updates,app_server_mcp_startup_next_round_keeps_terminal_statuses_after_starting
- MCP startup warning rendering and interaction state are covered part 2: codex-rs/tui/src/chatwidget/tests/mcp__startup.rs:app_server_mcp_startup_next_round_with_empty_expected_servers_reactivates,mcp_startup_complete_does_not_clear_running_task,mcp_startup_complete_preserves_review_status,mcp_startup_dedupes_same_round_duplicate_failure_warning,mcp_startup_failure_restores_running_status_header,mcp_startup_header_booting_snapshot,turn_start_preserves_active_mcp_startup_header,turn_start_replaces_idle_completed_mcp_startup_header
- MCP elicitation form parsing and approval metadata are covered: codex-rs/tui/src/bottom_pane/mcp__server_elicitation_tests.rs:empty_object_schema_uses_approval_actions,empty_tool_approval_schema_uses_approval_actions,parses_boolean_form_request,plugin_tool_suggestion_meta_without_install_url_is_parsed_into_request_payload,tool_approval_display_params_prefer_explicit_display_order,tool_suggestion_meta_is_parsed_into_request_payload,unsupported_numeric_form_falls_back
- MCP elicitation response and queue state are covered: codex-rs/tui/src/bottom_pane/mcp__server_elicitation_tests.rs:ctrl_c_cancels_elicitation,empty_tool_approval_schema_always_allow_sets_persist_meta,empty_tool_approval_schema_session_choice_sets_persist_meta,horizontal_list_keys_move_between_select_fields,queues_requests_fifo,resolved_request_dismisses_overlay_without_emitting_events,submit_sends_accept_with_typed_content
- MCP elicitation form rendering is covered: codex-rs/tui/src/bottom_pane/mcp__server_elicitation_tests.rs:approval_form_tool_approval_snapshot,approval_form_tool_approval_with_param_summary_snapshot,approval_form_tool_approval_with_persist_options_snapshot,boolean_form_snapshot,message_only_form_snapshot,message_only_form_with_persist_options_snapshot

### login-auth (auth and login behavior)

#### Description

MCP auth elicitation is MCP-server interaction state, not Codex login, logout, token refresh,
credential selection, or cached auth behavior.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Codex-as-MCP-server coverage should exercise external MCP client tool schemas, exec and patch
approval behavior, and MCP-compatible tool result serialization.

#### Test cases

- Codex-as-MCP-server tool behavior is covered: codex-rs/mcp-server/tests/suite/mcp__codex_tool.rs:test_codex_tool_passes_base_instructions,test_patch_approval_triggers_elicitation,test_shell_command_approval_triggers_elicitation

### rmcp-client (MCP client transport and resource behavior)

#### Description

MCP client coverage should exercise lower-level resource and streamable HTTP client behavior used
by the MCP feature.

#### Test cases

- RMCP process cleanup behavior is covered: codex-rs/rmcp-client/tests/mcp__process_group_cleanup.rs:drop_kills_wrapper_process_group,shutdown_kills_initialized_stdio_server_with_in_flight_operation
- RMCP resource behavior is covered: codex-rs/rmcp-client/tests/mcp__resources.rs:rmcp_client_can_list_and_read_resources
- RMCP streamable HTTP OAuth startup behavior is covered: codex-rs/rmcp-client/tests/mcp__streamable_http_oauth_startup.rs:oauth_startup_child,refreshes_expired_persisted_token_before_initialize
- RMCP streamable HTTP remote behavior is covered: codex-rs/rmcp-client/tests/mcp__streamable_http_remote.rs:streamable_http_remote_client_round_trips_through_exec_server
- RMCP streamable HTTP recovery behavior is covered: codex-rs/rmcp-client/tests/mcp__streamable_http_recovery.rs:streamable_http_401_does_not_trigger_recovery,streamable_http_403_finds_bearer_challenge_in_later_header_value,streamable_http_403_scope_challenge_returns_insufficient_scope,streamable_http_404_recovery_only_retries_once,streamable_http_404_session_expiry_recovers_and_retries_once,streamable_http_non_session_failure_does_not_trigger_recovery

### codex-api (Codex API client and protocol behavior)

#### Description

MCP does not change lower-level Codex API client or protocol behavior in the codex-api crate.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Exec CLI coverage should exercise non-interactive execution behavior when required MCP servers
cannot start.

#### Test cases

- Required MCP server exec failure behavior is covered: codex-rs/exec/tests/suite/mcp__required_exit.rs:exits_non_zero_when_required_mcp_server_fails_to_initialize

### otel (telemetry and export behavior)

#### Description

MCP does not currently define telemetry, metric, or export contract changes.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

MCP does not change exec-server process, filesystem, HTTP, relay, or WebSocket behavior.

#### Status

Not covered

## Test Generation Notes

Generate tests for server startup success/failure, status refresh, tool exposure bounds, tool-call
identity preservation, resource listing and reading, elicitation request/response correlation,
sensitive-value masking, CLI server management, and Codex MCP server approval behavior.
