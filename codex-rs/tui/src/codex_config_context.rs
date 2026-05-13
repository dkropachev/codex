use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::slash_command::built_in_slash_commands;

const CODEX_CONFIG_DONE_MARKER: &str = "<codex_config_done>";
const CODEX_GUIDE: &str = include_str!("../codex_guide.md");
const PLAN_MODE_GUIDE: &str = include_str!("../../collaboration-mode-templates/templates/plan.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexRequestMode {
    Investigate,
    ConfigEdit,
    AiResolve,
}

const CONFIG_EDIT_VERBS: &[&str] = &[
    "fix",
    "amend",
    "change",
    "update",
    "set",
    "enable",
    "disable",
    "add",
    "remove",
    "configure",
    "write",
    "save",
    "persist",
    "apply",
    "implement",
    "stop",
    "prevent",
    "avoid",
    "skip",
    "exclude",
    "omit",
];

const CONFIG_EDIT_PHRASES: &[&str] = &[
    "don t want",
    "dont want",
    "do not want",
    "should not",
    "must not",
    "no longer",
];

const INVESTIGATE_WORDS: &[&str] = &["investigate", "look", "inspect", "check", "show", "explain"];

const INVESTIGATE_PHRASES: &[&str] = &[
    "take a look",
    "without config update",
    "without updating config",
    "do not change config",
    "don t change config",
    "no config update",
];

pub(crate) fn classify_codex_request(request: &str) -> CodexRequestMode {
    let normalized = normalize_request(request);
    let words = normalized.split_whitespace().collect::<Vec<_>>();

    if contains_any_phrase(&normalized, INVESTIGATE_PHRASES)
        || words.iter().any(|word| INVESTIGATE_WORDS.contains(word))
    {
        return CodexRequestMode::Investigate;
    }

    if contains_any_phrase(&normalized, CONFIG_EDIT_PHRASES)
        || words.iter().any(|word| CONFIG_EDIT_VERBS.contains(word))
    {
        CodexRequestMode::ConfigEdit
    } else {
        CodexRequestMode::AiResolve
    }
}

fn normalize_request(request: &str) -> String {
    request
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_any_phrase(request: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| request.contains(phrase))
}

pub(crate) fn codex_config_workspace_for_cwd(cwd: &Path) -> PathBuf {
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

fn codex_config_plan_sandbox_policy() -> SandboxPolicy {
    SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: false,
    }
}

pub(crate) fn codex_config_plan_permission_profile() -> PermissionProfile {
    PermissionProfile::from_legacy_sandbox_policy(&codex_config_plan_sandbox_policy())
}

fn codex_config_edit_sandbox_policy(codex_home: &AbsolutePathBuf) -> SandboxPolicy {
    SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![codex_home.clone()],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: false,
    }
}

pub(crate) fn codex_config_edit_permission_profile(
    codex_home: &AbsolutePathBuf,
) -> PermissionProfile {
    PermissionProfile::from_legacy_sandbox_policy(&codex_config_edit_sandbox_policy(codex_home))
}

pub(crate) fn codex_permission_profile_for_mode(
    collaboration_mode: &CollaborationMode,
    codex_home: &AbsolutePathBuf,
) -> PermissionProfile {
    if collaboration_mode.mode == ModeKind::CodexConfigEdit {
        codex_config_edit_permission_profile(codex_home)
    } else {
        codex_config_plan_permission_profile()
    }
}

pub(crate) fn codex_investigate_mask(target_cwd: &Path) -> CollaborationModeMask {
    let workspace = codex_config_workspace_for_cwd(target_cwd);
    CollaborationModeMask {
        name: ModeKind::Codex.display_name().to_string(),
        mode: Some(ModeKind::Codex),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_codex_investigate_context(
            target_cwd, &workspace,
        ))),
    }
}

pub(crate) fn codex_config_edit_mask(
    target_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> CollaborationModeMask {
    let workspace = codex_config_workspace_for_cwd(target_cwd);
    CollaborationModeMask {
        name: ModeKind::Codex.display_name().to_string(),
        mode: Some(ModeKind::CodexConfigEdit),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_codex_config_edit_context(
            target_cwd, &workspace, codex_home,
        ))),
    }
}

pub(crate) fn codex_ai_resolve_mask(
    target_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> CollaborationModeMask {
    let workspace = codex_config_workspace_for_cwd(target_cwd);
    CollaborationModeMask {
        name: ModeKind::Codex.display_name().to_string(),
        mode: Some(ModeKind::CodexConfigEdit),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_codex_ai_resolve_context(
            target_cwd, &workspace, codex_home,
        ))),
    }
}

fn build_codex_investigate_context(target_cwd: &Path, workspace: &Path) -> String {
    let mut sections = vec![format!(
        "# Codex Investigate Mode\n\nTarget workspace/repository, read-only for this mode: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nYou are investigating Codex itself. Use the guide, slash-command registry, CLI help, tools, app-server APIs, and source inspection to answer questions about Codex configuration and behavior. Do not change configuration, do not write to the target workspace, and do not emit a `<proposed_plan>` block. When the investigation answer is complete and the TUI should offer the post-turn Codex prompt, put `{CODEX_CONFIG_DONE_MARKER}` on its own line at the end of the final answer. Do not ask whether to continue, stop, or exit Codex mode in visible text. If you need files or scripts, create them under the scratch workspace.",
        target_cwd.display(),
        workspace.display()
    )];

    push_shared_context(&mut sections);
    sections.join("\n\n")
}

fn build_codex_config_edit_context(
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> String {
    let mut sections = vec![
        PLAN_MODE_GUIDE.to_string(),
        format!(
            "# Codex Config Edit Mode\n\nTarget workspace/repository, read-only while planning or applying: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory in edit mode: `{}`\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nWork like Plan Mode for new or unapproved configuration requests: explore and inspect as needed, but do not mutate files, config, app-server state, plugins, skills, MCP/apps, memories, repo-ci, model-router, tool-router, or any other Codex state until the user accepts the proposed plan. Do not attempt writes to prove they are blocked. When ready, emit exactly one complete `<proposed_plan>` block describing the config changes, validation, refresh/reload steps, and rollback considerations.\n\nIf the visible user message asks to apply or implement an already accepted Codex config plan, enter edit mode for that turn: apply only the approved Codex configuration changes, write only under the Codex config directory or `/tmp`, do not modify the target workspace/repository, then validate and reload or describe any required restart. Do not emit a new `<proposed_plan>` for an apply turn. Do not ask whether to continue, stop, or exit Codex mode in the final answer.",
            target_cwd.display(),
            workspace.display(),
            codex_home.display()
        ),
    ];

    push_shared_context(&mut sections);
    sections.join("\n\n")
}

fn build_codex_ai_resolve_context(
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> String {
    let mut sections = vec![
        PLAN_MODE_GUIDE.to_string(),
        format!(
            "# Codex AI Classification Fallback Mode\n\nTarget workspace/repository, read-only while resolving this request: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nCodex config directory for approved edit turns: `{}`\n\nThe local deterministic `/codex` classifier could not confidently classify the user-authored request as investigation or config-edit. The user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nFirst classify the user request using the runtime context you inspect:\n- If the request is asking what Codex currently does, how something works, or for a read-only diagnosis, answer in Codex investigate style: do not change configuration, do not write to the target workspace, do not emit a `<proposed_plan>` block, and put `{CODEX_CONFIG_DONE_MARKER}` on its own line at the end of the final answer.\n- If the request asks Codex behavior/configuration to change, enter Codex config-edit planning style: follow Plan Mode, do not mutate files, config, app-server state, plugins, skills, MCP/apps, memories, repo-ci, model-router, tool-router, or any other Codex state until the user accepts a proposed plan, and emit exactly one complete `<proposed_plan>` block. Applying it happens in a later edit turn with the Codex config directory writable.\n- If inspection still cannot resolve the intent, ask a concise clarifying question instead of guessing or mutating state.",
            target_cwd.display(),
            workspace.display(),
            codex_home.display()
        ),
    ];

    push_shared_context(&mut sections);
    sections.join("\n\n")
}

fn push_shared_context(sections: &mut Vec<String>) {
    sections.push(format!("Codex guide:\n```markdown\n{CODEX_GUIDE}\n```"));

    sections.push(format!(
        "CLI help generated from `codex --help`:\n```text\n{}\n```",
        codex_help()
    ));

    sections.push(slash_command_context());
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

    lines.push(format!("Bare /codex enters persistent Codex investigate mode. /codex <request> classifies the request: read-only investigation stays in Codex investigate mode and ends with a hidden `{CODEX_CONFIG_DONE_MARKER}` marker for the leave/stay prompt, explicit config-edit requests enter Codex config-edit planning mode and must produce a <proposed_plan> before any mutation, and ambiguous requests enter an AI classification fallback that chooses one of those two behaviors before answering."));

    lines.join("\n")
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
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn classify_codex_request_prefers_explicit_investigation() {
        assert_eq!(
            classify_codex_request("show how to update repo-ci without config update"),
            CodexRequestMode::Investigate
        );
        assert_eq!(
            classify_codex_request("do not change config; explain how to enable memories"),
            CodexRequestMode::Investigate
        );
    }

    #[test]
    fn classify_codex_request_detects_config_edit_verbs() {
        assert_eq!(
            classify_codex_request("update repo-ci defaults"),
            CodexRequestMode::ConfigEdit
        );
        assert_eq!(
            classify_codex_request(
                "i don't want repo-ci to run cibuildwheel or integration tests at all"
            ),
            CodexRequestMode::ConfigEdit
        );
    }

    #[test]
    fn classify_codex_request_uses_ai_fallback_for_ambiguous_prompts() {
        assert_eq!(
            classify_codex_request("repo-ci cibuildwheel integration tests"),
            CodexRequestMode::AiResolve
        );
    }
}
