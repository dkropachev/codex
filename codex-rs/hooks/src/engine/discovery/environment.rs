//! Discovers and validates command hooks owned by an executor environment.

use super::DiscoveryMode;
use super::DiscoveryResult;
use super::HookDiscoveryPolicy;
use super::HookHandlerSource;
use super::HookRequirement;
use super::append_hook_events;
use super::ensure_required_hooks_loaded;
use super::synthetic_layer_path;
use crate::engine::HandlerSourcePath;
use codex_config::HookEventsToml;
use codex_config::HookHandlerConfig;
use codex_protocol::protocol::HookSource;
use codex_utils_path_uri::PathConvention;
use codex_utils_path_uri::PathUri;
use std::collections::HashMap;

pub(crate) fn discover_environment_hooks(
    sources: Vec<crate::EnvironmentHookSource>,
    mut display_order: i64,
) -> DiscoveryResult {
    let mut discovered = DiscoveryResult::default();
    for source in sources {
        append_environment_hooks(&mut discovered, source, &mut display_order);
    }
    discovered
}

/// Reject unloadable requirements before accepting an environment's configuration.
pub fn validate_environment_hooks(
    environment_id: &str,
    cwd: &PathUri,
    hooks: &HookEventsToml,
) -> anyhow::Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }
    let source = crate::EnvironmentHookSource {
        environment_id: environment_id.to_owned(),
        cwd: cwd.clone(),
        hooks: hooks.clone(),
    };
    let discovered = discover_environment_hooks(vec![source], /*display_order*/ 0);
    ensure_required_hooks_loaded(&discovered.required_load_errors)
}

fn append_environment_hooks(
    discovered: &mut DiscoveryResult,
    mut source: crate::EnvironmentHookSource,
    display_order: &mut i64,
) {
    let path = synthetic_layer_path(&format!(
        "<environment:{}>/requirements.toml",
        source.environment_id
    ));
    // Resolve overrides against the executor platform before normal local discovery.
    for (_, groups) in source.hooks.matcher_groups_mut() {
        for handler in groups.iter_mut().flat_map(|group| &mut group.hooks) {
            if let HookHandlerConfig::Command {
                command,
                command_windows,
                ..
            } = handler
                && let Some(windows) = command_windows.take()
                && source.cwd.infer_path_convention() == Some(PathConvention::Windows)
            {
                *command = windows;
            }
        }
    }
    let handler_start = discovered.handlers.len();
    let entry_start = discovered.hook_entries.len();
    append_hook_events(
        &mut discovered.handlers,
        &mut discovered.hook_entries,
        &mut discovered.warnings,
        display_order,
        HookHandlerSource {
            path: &path,
            key_source: path.display().to_string(),
            source: HookSource::System,
            is_managed: true,
            requirement: HookRequirement::Required(&mut discovered.required_load_errors),
            bypass_hook_trust: false,
            hook_states: &HashMap::new(),
            env: HashMap::new(),
            plugin_id: None,
            plugin_content_hash: None,
            config_layer_stack: None,
            plugin_root: None,
        },
        source.hooks,
        HookDiscoveryPolicy {
            config_layer_stack: None,
            mode: DiscoveryMode::Runtime,
            allow_managed_hooks_only: true,
            bypass_hook_trust: false,
        },
    );
    for entry in &mut discovered.hook_entries[entry_start..] {
        entry.environment_id = Some(source.environment_id.clone());
    }
    for handler in &mut discovered.handlers[handler_start..] {
        handler.source_path = HandlerSourcePath::Environment {
            environment_id: source.environment_id.clone(),
            cwd: source.cwd.clone(),
            source_path: path.clone(),
        };
    }
}
