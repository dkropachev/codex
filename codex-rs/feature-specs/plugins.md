# Plugins

## Summary

Plugins let users extend Codex with packaged capabilities, local or remote manifests, marketplace
entries, plugin-provided instructions, plugin mentions, and installable tools. Plugin behavior is
user-facing through CLI commands, TUI plugin surfaces, app-server APIs, and model-visible context.

## Behavior

Installed plugins are discovered from configured plugin locations and marketplace metadata. Plugin
manifests define identity and bundled content. Users can list, read, install, uninstall, share, and
upgrade plugins through supported surfaces.

Marketplace operations add, remove, and upgrade installable plugin sources. Remote plugin state can
be synchronized and cached, but user-visible plugin lists must represent the current configured and
installed plugin set.

Plugin instructions and mentions can become model-visible context. That context must stay bounded,
be tied to explicit plugin availability, and avoid injecting stale or disabled plugin content.

## Entry Points

- [codex-rs/core-plugins/src/manager.rs](../core-plugins/src/manager.rs)
- [codex-rs/core-plugins/src/manifest.rs](../core-plugins/src/manifest.rs)
- [codex-rs/core-plugins/src/marketplace.rs](../core-plugins/src/marketplace.rs)
- [codex-rs/core/src/plugins/mod.rs](../core/src/plugins/mod.rs)
- [codex-rs/core/src/context/plugin_instructions.rs](../core/src/context/plugin_instructions.rs)
- [codex-rs/app-server-protocol/src/protocol/v2/plugin.rs](../app-server-protocol/src/protocol/v2/plugin.rs)
- [codex-rs/app-server/src/request_processors/plugins.rs](../app-server/src/request_processors/plugins.rs)
- [codex-rs/cli/src/plugin_cmd.rs](../cli/src/plugin_cmd.rs)
- [codex-rs/cli/src/marketplace_cmd.rs](../cli/src/marketplace_cmd.rs)
- [codex-rs/tui/src/chatwidget/plugins.rs](../tui/src/chatwidget/plugins.rs)

## Subfeatures

### Installed Plugins

#### Entry Points

- [codex-rs/core-plugins/src/manager.rs](../core-plugins/src/manager.rs)
- [codex-rs/core-plugins/src/loader.rs](../core-plugins/src/loader.rs)
- [codex-rs/core-plugins/src/store.rs](../core-plugins/src/store.rs)
- [codex-rs/app-server/src/request_processors/plugins.rs](../app-server/src/request_processors/plugins.rs)

#### Invariants

- Plugin identity is derived from manifest-backed plugin IDs.
- Invalid plugins are reported without preventing valid plugins from loading.
- List/read APIs reflect installed plugin state without requiring a model turn.

### Marketplace

#### Entry Points

- [codex-rs/core-plugins/src/marketplace.rs](../core-plugins/src/marketplace.rs)
- [codex-rs/core-plugins/src/marketplace_add.rs](../core-plugins/src/marketplace_add.rs)
- [codex-rs/core-plugins/src/marketplace_remove.rs](../core-plugins/src/marketplace_remove.rs)
- [codex-rs/core-plugins/src/marketplace_upgrade.rs](../core-plugins/src/marketplace_upgrade.rs)
- [codex-rs/app-server/src/request_processors/marketplace_processor.rs](../app-server/src/request_processors/marketplace_processor.rs)
- [codex-rs/cli/src/marketplace_cmd.rs](../cli/src/marketplace_cmd.rs)

#### Invariants

- Marketplace add/remove/upgrade operations update the configured marketplace set deterministically.
- Upgrade reports preserve enough detail for users to understand changed plugin availability.
- Remote marketplace failures do not corrupt existing installed plugin state.

### Plugin Model Context

#### Entry Points

- [codex-rs/core/src/plugins/injection.rs](../core/src/plugins/injection.rs)
- [codex-rs/core/src/plugins/render.rs](../core/src/plugins/render.rs)
- [codex-rs/core/src/plugins/mentions.rs](../core/src/plugins/mentions.rs)
- [codex-rs/core/src/context/available_plugins_instructions.rs](../core/src/context/available_plugins_instructions.rs)
- [codex-rs/core/src/context/plugin_instructions.rs](../core/src/context/plugin_instructions.rs)
- [codex-rs/tui/src/app/plugin_mentions.rs](../tui/src/app/plugin_mentions.rs)

#### Invariants

- Model-visible plugin context is bounded.
- Disabled or unavailable plugins are not injected as active plugin instructions.
- Plugin mentions resolve only to available plugin IDs.

## Invariants

- Plugin manifests are the source of plugin identity and packaged capabilities.
- Plugin install/list/read/share/uninstall operations are available without starting a model turn.
- Plugin model context is bounded and derived from enabled, discoverable plugins.
- Marketplace state changes do not silently discard existing installed plugin state.

## Test Places

### agent-e2e (agent behavior under core integration tests)

#### Description

Agent coverage should exercise plugin discovery, bounded model-context injection, plugin
mention/request flows, disabled plugin exclusion, and request-to-install behavior during agent
turns.

#### Test cases

- Plugin model-context behavior is covered: codex-rs/core/tests/suite/plugins__agent_context.rs:capability_sections_render_in_developer_message_in_order,explicit_plugin_mentions_inject_plugin_guidance,explicit_plugin_mentions_track_plugin_used_analytics
- Plugin request-to-install behavior is covered: codex-rs/core/tests/suite/plugins__request_install.rs:request_plugin_install_is_available_without_search_tool_after_discovery_attempts

### app-server-api (app-server API behavior)

#### Description

App-server coverage should exercise plugin list, read, install, uninstall, share, and marketplace
add, remove, and upgrade APIs.

#### Test cases

- Plugin install API behavior is covered part 1: codex-rs/app-server/tests/suite/v2/plugins__install.rs:plugin_install_errors_when_remote_bundle_download_fails,plugin_install_filters_disallowed_apps_needing_auth,plugin_install_makes_bundled_mcp_servers_available_to_followup_requests,plugin_install_rejects_invalid_remote_plugin_name,plugin_install_rejects_invalid_remote_release_version,plugin_install_rejects_missing_install_source,plugin_install_rejects_missing_remote_bundle_url,plugin_install_rejects_multiple_install_sources
- Plugin install API behavior is covered part 2: codex-rs/app-server/tests/suite/v2/plugins__install.rs:plugin_install_rejects_plain_http_remote_bundle_url,plugin_install_rejects_relative_marketplace_paths,plugin_install_rejects_remote_marketplace_when_plugins_are_disabled,plugin_install_rejects_remote_plugin_disabled_by_admin_before_download,plugin_install_rejects_when_workspace_codex_plugins_disabled,plugin_install_returns_apps_needing_auth,plugin_install_returns_invalid_request_for_disallowed_product_plugin,plugin_install_returns_invalid_request_for_missing_marketplace_file
- Plugin install API behavior is covered part 3: codex-rs/app-server/tests/suite/v2/plugins__install.rs:plugin_install_returns_invalid_request_for_not_available_plugin,plugin_install_tracks_analytics_event,plugin_install_tracks_remote_plugin_analytics_event,plugin_install_writes_remote_plugin_to_cloud_and_cache
- Plugin list API behavior is covered part 1: codex-rs/app-server/tests/suite/v2/plugins__list.rs:app_server_startup_sync_downloads_remote_installed_plugin_bundles,plugin_installed_ignores_local_cache_without_catalog,plugin_installed_includes_installed_plugins_and_explicit_install_suggestions,plugin_installed_includes_remote_shared_with_me_plugins,plugin_installed_prefers_remote_curated_conflicts_when_remote_plugin_enabled,plugin_installed_starts_remote_installed_bundle_sync,plugin_list_accepts_legacy_string_default_prompt,plugin_list_accepts_omitted_cwds
- Plugin list API behavior is covered part 2: codex-rs/app-server/tests/suite/v2/plugins__list.rs:plugin_list_does_not_append_global_remote_when_marketplace_kinds_are_explicit,plugin_list_does_not_fetch_remote_marketplaces_when_plugins_disabled,plugin_list_does_not_query_openai_curated_remote_collection_by_default,plugin_list_fail_opens_openai_curated_remote_collection_errors,plugin_list_fetches_featured_plugin_ids_without_chatgpt_auth,plugin_list_fetches_shared_with_me_kind,plugin_list_fetches_workspace_directory_kind_without_remote_plugin_flag,plugin_list_includes_install_and_enabled_state_from_config
- Plugin list API behavior is covered part 3: codex-rs/app-server/tests/suite/v2/plugins__list.rs:plugin_list_includes_openai_curated_remote_collection_when_requested,plugin_list_includes_remote_marketplaces_when_remote_plugin_enabled,plugin_list_keeps_valid_marketplaces_when_another_marketplace_fails_to_load,plugin_list_marks_remote_plugin_disabled_by_admin,plugin_list_omits_shared_with_me_kind_when_plugin_sharing_disabled,plugin_list_rejects_relative_cwds,plugin_list_returns_empty_when_workspace_codex_plugins_disabled,plugin_list_returns_installed_git_source_interface_from_cache
- Plugin list API behavior is covered part 4: codex-rs/app-server/tests/suite/v2/plugins__list.rs:plugin_list_returns_plugin_interface_with_absolute_asset_paths,plugin_list_returns_share_context_for_shared_local_plugin,plugin_list_reuses_cached_workspace_codex_plugins_setting,plugin_list_skips_invalid_marketplace_file_and_reports_error,plugin_list_sync_upgrades_and_removes_remote_installed_plugin_bundles,plugin_list_uses_alternate_discoverable_manifest_and_keeps_undiscoverable_plugins,plugin_list_uses_cached_global_remote_catalog_and_refreshes_it,plugin_list_uses_home_config_for_enabled_state
- Plugin list API behavior is covered part 5: codex-rs/app-server/tests/suite/v2/plugins__list.rs:plugin_list_uses_warmed_featured_plugin_ids_cache_on_first_request,plugin_list_vertical_kind_noops_when_remote_plugin_enabled
- Marketplace add behavior is covered: codex-rs/app-server/tests/suite/v2/plugins__marketplace_add.rs:marketplace_add_local_directory_source
- Marketplace remove behavior is covered: codex-rs/app-server/tests/suite/v2/plugins__marketplace_remove.rs:marketplace_remove_deletes_config_and_installed_root,marketplace_remove_rejects_unknown_marketplace
- Marketplace upgrade behavior is covered: codex-rs/app-server/tests/suite/v2/plugins__marketplace_upgrade.rs:marketplace_upgrade_all_configured_git_marketplaces,marketplace_upgrade_named_marketplace_only,marketplace_upgrade_rejects_unknown_or_non_git_marketplace,marketplace_upgrade_returns_empty_roots_when_already_up_to_date
- Plugin read API behavior is covered part 1: codex-rs/app-server/tests/suite/v2/plugins__read.rs:plugin_read_accepts_legacy_string_default_prompt,plugin_read_describes_uninstalled_git_source_without_cloning,plugin_read_fails_on_malformed_share_mapping,plugin_read_falls_back_to_local_share_context_without_remote_auth,plugin_read_keeps_remote_version_when_share_principals_are_missing,plugin_read_maps_missing_remote_plugin_to_invalid_request,plugin_read_reads_remote_plugin_details_when_remote_plugin_enabled,plugin_read_rejects_invalid_remote_plugin_name
- Plugin read API behavior is covered part 2: codex-rs/app-server/tests/suite/v2/plugins__read.rs:plugin_read_rejects_missing_read_source,plugin_read_rejects_multiple_read_sources,plugin_read_rejects_remote_marketplace_when_plugins_are_disabled,plugin_read_returns_app_needs_auth,plugin_read_returns_canonical_openai_curated_marketplace_name,plugin_read_returns_invalid_request_when_plugin_is_missing,plugin_read_returns_invalid_request_when_plugin_manifest_is_missing,plugin_read_returns_plugin_details_with_bundle_contents
- Plugin read API behavior is covered part 3: codex-rs/app-server/tests/suite/v2/plugins__read.rs:plugin_read_returns_remote_mcp_servers_when_uninstalled,plugin_read_returns_share_context_for_shared_local_plugin,plugin_read_returns_share_context_for_shared_remote_plugin,plugin_skill_read_reads_remote_skill_contents_when_remote_plugin_enabled
- Plugin share API behavior is covered part 1: codex-rs/app-server/tests/suite/v2/plugins__share.rs:plugin_share_checkout_adds_personal_marketplace_entry,plugin_share_checkout_cleans_up_path_when_marketplace_update_fails,plugin_share_checkout_rejects_non_share_remote_plugin,plugin_share_delete_removes_created_workspace_plugin,plugin_share_list_returns_created_workspace_plugins,plugin_share_rejects_workspace_targets_from_client,plugin_share_save_forwards_access_policy,plugin_share_save_rejects_access_policy_for_existing_plugin
- Plugin share API behavior is covered part 2: codex-rs/app-server/tests/suite/v2/plugins__share.rs:plugin_share_save_rejects_listed_discoverability,plugin_share_save_rejects_when_plugin_sharing_disabled,plugin_share_save_uploads_local_plugin,plugin_share_update_targets_rejects_when_plugin_sharing_disabled,plugin_share_update_targets_updates_share_targets
- Plugin uninstall API behavior is covered part 1: codex-rs/app-server/tests/suite/v2/plugins__uninstall.rs:plugin_uninstall_accepts_workspace_remote_plugin_id_shape,plugin_uninstall_rejects_before_post_when_remote_detail_fetch_fails,plugin_uninstall_rejects_empty_remote_plugin_id,plugin_uninstall_rejects_invalid_remote_plugin_id_before_network_call,plugin_uninstall_rejects_remote_plugin_id_with_spaces_before_network_call,plugin_uninstall_rejects_remote_plugin_when_plugins_are_disabled,plugin_uninstall_removes_plugin_cache_and_config_entry,plugin_uninstall_tracks_analytics_event
- Plugin uninstall API behavior is covered part 2: codex-rs/app-server/tests/suite/v2/plugins__uninstall.rs:plugin_uninstall_uses_detail_scope_for_cache_namespace,plugin_uninstall_writes_remote_plugin_to_cloud_when_remote_plugin_enabled

### cli (main CLI command behavior)

#### Description

CLI coverage should exercise plugin commands and marketplace commands.

#### Test cases

- Plugin CLI behavior is covered part 1: codex-rs/cli/tests/plugins__cli.rs:marketplace_list_fails_when_configured_local_marketplace_source_is_missing,marketplace_list_fails_when_configured_marketplace_name_is_invalid,marketplace_list_fails_when_configured_marketplace_snapshot_is_malformed,marketplace_list_fails_when_configured_marketplace_snapshot_is_missing,marketplace_list_fails_when_home_marketplace_is_malformed,marketplace_list_includes_home_marketplace_when_present,marketplace_list_includes_root_when_plugins_are_filtered_out,marketplace_list_json_includes_configured_git_marketplace_source
- Plugin CLI behavior is covered part 2: codex-rs/cli/tests/plugins__cli.rs:marketplace_list_json_keys_configured_source_by_root,marketplace_list_json_prints_configured_marketplaces,marketplace_list_shows_configured_marketplace_names,plugin_add_and_remove_updates_installed_plugin_config,plugin_add_fails_when_configured_marketplace_snapshot_is_malformed,plugin_add_json_prints_install_outcome,plugin_add_reinstalls_from_configured_marketplace_snapshot,plugin_add_rejects_cached_plugins_without_authorizing_marketplace_snapshot
- Plugin CLI behavior is covered part 3: codex-rs/cli/tests/plugins__cli.rs:plugin_add_rejects_unconfigured_repo_local_marketplaces,plugin_list_available_requires_json,plugin_list_excludes_unconfigured_repo_local_marketplaces,plugin_list_fails_for_custom_marketplace_under_system_root,plugin_list_fails_when_configured_marketplace_snapshot_is_missing,plugin_list_hides_version_for_cached_but_unconfigured_plugin,plugin_list_ignores_implicit_system_marketplace_roots_without_manifests,plugin_list_json_includes_configured_git_marketplace_source
- Plugin CLI behavior is covered part 4: codex-rs/cli/tests/plugins__cli.rs:plugin_list_json_prints_available_plugins_when_requested,plugin_list_json_prints_installed_plugins,plugin_list_prints_plugins_in_a_table,plugin_list_shows_installed_version_when_plugin_is_installed,plugin_remove_json_prints_remove_outcome,plugin_remove_works_after_marketplace_is_removed
- Marketplace add behavior is covered: codex-rs/cli/tests/plugins__marketplace_add.rs:marketplace_add_json_prints_add_outcome,marketplace_add_local_directory_source,marketplace_add_rejects_local_manifest_file_source,marketplace_add_rejects_sparse_for_local_directory_source
- Marketplace remove behavior is covered: codex-rs/cli/tests/plugins__marketplace_remove.rs:marketplace_remove_deletes_config_and_installed_root,marketplace_remove_json_prints_remove_outcome,marketplace_remove_rejects_unknown_marketplace
- Marketplace upgrade behavior is covered: codex-rs/cli/tests/plugins__marketplace_upgrade.rs:marketplace_upgrade_json_prints_upgrade_outcome,marketplace_upgrade_no_longer_runs_at_top_level,marketplace_upgrade_runs_under_plugin

### tui-e2e (full terminal TUI behavior)

#### Description

Full TUI coverage should exercise user-facing plugin list, install, and plugin mention flows in a
live terminal session when those surfaces are available.

#### Test cases

- Live TUI plugin list and install flows are covered: missing
- Live TUI plugin mention selection and submission are covered: missing

### tui-component (focused TUI component behavior)

#### Description

Focused TUI coverage should exercise plugin mention resolution, selection state, disabled plugin
handling, and plugin UI rendering.

#### Test cases

- Plugin mention resolution and selection state are covered: missing
- Installed plugin enablement and disabled-state popup behavior are covered: codex-rs/tui/src/chatwidget/tests/plugins__popups.rs:plugins_popup_space_toggles_installed_plugin_from_list,plugins_popup_space_on_uninstalled_row_does_not_start_search,plugins_popup_space_with_active_search_does_not_toggle_installed_plugin

### login-auth (auth and login behavior)

#### Description

Plugin installation can report apps needing auth, but plugin behavior does not change Codex login,
logout, token refresh, or cached auth semantics.

#### Status

Not covered

### mcp-server (Codex-as-MCP-server behavior)

#### Description

Plugins can install MCP servers, but Codex-as-MCP-server tool behavior is owned by the MCP feature.

#### Status

Not covered

### rmcp-client (MCP client transport and resource behavior)

#### Description

Plugin tests should verify bundled MCP server availability through app-server and agent surfaces;
lower-level MCP transport remains owned by MCP.

#### Status

Not covered

### codex-api (Codex API client and protocol behavior)

#### Description

Plugins do not change the lower-level Codex API client or protocol behavior.

#### Status

Not covered

### exec-cli (codex exec CLI behavior)

#### Description

Plugin behavior is not exposed through non-interactive exec mode command behavior.

#### Status

Not covered

### otel (telemetry and export behavior)

#### Description

Plugin analytics are covered at the app-server behavior level; this feature spec does not define
otel exporter behavior.

#### Status

Not covered

### exec-server (exec-server service boundary behavior)

#### Description

Plugins do not change exec-server process, filesystem, HTTP, relay, or WebSocket behavior.

#### Status

Not covered

## Test Generation Notes

Generate tests for plugin discovery, invalid plugin reporting, install/list/read/uninstall/share,
marketplace add/remove/upgrade, plugin mention resolution, bounded model context injection, and
disabled plugin exclusion.
