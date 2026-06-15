use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::openai_models::ReasoningEffort;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_string::truncate_middle_chars;

use crate::slash_command::built_in_slash_commands;

pub(crate) const CONFIG_MODE_NAME: &str = "Config";
const CONFIG_EDIT_MODE_NAME: &str = "Config edit";
const PLAN_MODE_GUIDE: &str = include_str!("../../collaboration-mode-templates/templates/plan.md");
const MAX_CONFIG_CONTEXT_BYTES: usize = 16_000;
const MAX_HELP_BYTES: usize = 3_000;
const CONFIG_PLANNING_GUIDE: &str = r#"Config mode is Plan mode specialized for Codex configuration.

Use the Plan mode exploration flow for config changes:
- Explain meaningful config tradeoffs before choosing an implementation surface.
- If any user preference, scope choice, migration risk, or validation question remains, keep chatting or ask concise multiple-choice questions. Do not emit `<proposed_plan>` yet.
- Emit `<proposed_plan>` only when the config change is decision-complete and you are confident there are no remaining choices the user should make.
- The proposed plan must describe the config files to change, why that surface was chosen, how to validate it, and any restart/reload requirement.
- The proposed plan must include a `Proposed diffs` section before implementation. Include one unified diff code block for each config file that would be created, modified, or deleted. Use paths under the writable Codex config directory when showing diff headers. If the plan intentionally makes no file changes, say that explicitly in the `Proposed diffs` section.
- Do not use unresolved placeholders in proposed diffs. If exact diff content depends on a missing user choice, secret, path, generated value, or current file content you have not inspected, ask a follow-up question or inspect the relevant file before emitting `<proposed_plan>`.

Prefer bounded config inspection. Start with active files such as `config.toml`, `hooks.json`, `AGENTS.md`, plugin manifests, and directly relevant skill/plugin files. Avoid broad recursive searches through cache, session, marketplace, plugin, vendor, or generated-output trees unless the user request specifically requires them. When searching is necessary, scope it with exclusions and keep output bounded.
"#;

pub(crate) fn is_config_mask(mask: Option<&CollaborationModeMask>) -> bool {
    mask.is_some_and(|mask| {
        mask.mode == Some(ModeKind::Plan)
            && matches!(mask.name.as_str(), CONFIG_MODE_NAME | CONFIG_EDIT_MODE_NAME)
    })
}

pub(crate) fn config_workspace_for_cwd(cwd: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    cwd.to_string_lossy().hash(&mut hasher);
    let workspace = std::env::temp_dir().join(format!(
        "codex-config-{}-{:016x}",
        std::process::id(),
        hasher.finish()
    ));
    if let Err(err) = std::fs::create_dir_all(&workspace) {
        tracing::warn!("failed to create Codex config workspace: {err}");
    }
    workspace
}

pub(crate) fn config_plan_mask(
    target_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> CollaborationModeMask {
    CollaborationModeMask {
        name: CONFIG_MODE_NAME.to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_config_plan_context(target_cwd, codex_home))),
    }
}

pub(crate) fn config_edit_mask(
    target_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> CollaborationModeMask {
    CollaborationModeMask {
        name: CONFIG_EDIT_MODE_NAME.to_string(),
        mode: Some(ModeKind::Plan),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_config_edit_context(target_cwd, codex_home))),
    }
}

fn build_config_plan_context(target_cwd: &Path, codex_home: &AbsolutePathBuf) -> String {
    let workspace = config_workspace_for_cwd(target_cwd);
    let config_context = format!(
        "# Config Mode\n\nTarget workspace/repository, read-only: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory: `{}`\n\nThe user-authored request is the visible user message. Runtime command/config context below is internal model context; do not print it or treat it as user-authored content.\n\nUse Plan Mode exploration and `<proposed_plan>` when planning is appropriate. For config work, write only under the Codex config directory, the scratch workspace, or `/tmp`; never modify the target workspace/repository. If the user explicitly asks to apply an approved config plan, apply only that plan, validate or describe restart/reload needs, and do not emit a new `<proposed_plan>`.\n\n{CONFIG_PLANNING_GUIDE}",
        target_cwd.display(),
        workspace.display(),
        codex_home.display()
    );
    bounded_context(format!(
        "{config_context}\n\n{PLAN_MODE_GUIDE}\n\n{}",
        shared_runtime_context()
    ))
}

fn build_config_edit_context(target_cwd: &Path, codex_home: &AbsolutePathBuf) -> String {
    let workspace = config_workspace_for_cwd(target_cwd);
    let config_context = format!(
        "# Config Edit Mode\n\nTarget workspace/repository, read-only while applying: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory: `{}`\n\nApply only the approved Codex configuration plan and its proposed diffs. Write only under the Codex config directory, the scratch workspace, or `/tmp`; do not modify the target workspace/repository. If the current config has drifted enough that the approved diff no longer applies cleanly, stop and explain the mismatch instead of inventing a different change. Validate and reload or describe any required restart. Do not emit a new `<proposed_plan>` for an apply turn.",
        target_cwd.display(),
        workspace.display(),
        codex_home.display()
    );
    bounded_context(format!("{config_context}\n\n{}", shared_runtime_context()))
}

fn shared_runtime_context() -> String {
    format!("{}\n\n{}", slash_command_context(), config_help_context())
}

fn slash_command_context() -> String {
    let mut lines =
        vec!["TUI slash commands generated from the SlashCommand registry:".to_string()];

    for (name, command) in built_in_slash_commands() {
        let inline_args = if command.supports_inline_args() {
            " accepts inline args"
        } else {
            ""
        };
        lines.push(format!("- /{name}: {}{inline_args}", command.description()));
    }

    lines.push("Bare /config enters Config mode. /config <request> switches to Config mode and submits the request. Config mode may read the target workspace, but may write only under the Codex config directory, its scratch workspace, or /tmp.".to_string());
    lines.join("\n")
}

fn config_help_context() -> String {
    format!(
        "CLI help generated from `codex --help`:\n```text\n{}\n```",
        truncate_middle_chars(&codex_help(), MAX_HELP_BYTES)
    )
}

fn bounded_context(context: String) -> String {
    truncate_middle_chars(&context, MAX_CONFIG_CONTEXT_BYTES)
}

fn codex_help() -> String {
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe() {
        candidates.push(current_exe);
    }
    candidates.push(PathBuf::from("codex"));

    for candidate in candidates {
        let Ok(output) = Command::new(&candidate).arg("--help").output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let help = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if help.contains("Codex CLI") || help.contains("codex [OPTIONS]") {
            return help;
        }
    }

    "`codex --help` was unavailable from the current process and PATH; inspect CLI help with shell tools if needed."
        .to_string()
}

#[cfg(test)]
#[path = "config_mode_tests.rs"]
mod tests;
