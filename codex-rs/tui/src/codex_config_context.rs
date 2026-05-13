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

const CODEX_GUIDE: &str = include_str!("../codex_guide.md");
const PLAN_MODE_GUIDE: &str = include_str!("../../collaboration-mode-templates/templates/plan.md");

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

pub(crate) fn codex_config_plan_mask(
    target_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> CollaborationModeMask {
    let workspace = codex_config_workspace_for_cwd(target_cwd);
    CollaborationModeMask {
        name: ModeKind::Codex.display_name().to_string(),
        mode: Some(ModeKind::Codex),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_codex_config_plan_context(
            target_cwd, &workspace, codex_home,
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

fn build_codex_config_plan_context(
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> String {
    let mut sections = vec![
        PLAN_MODE_GUIDE.to_string(),
        format!(
            "# Codex Config Planning Mode\n\nTarget workspace/repository, read-only while planning: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nCodex config directory, read-only until the plan is approved: `{}`\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nWork exactly like Plan Mode, but plan changes to Codex configuration and local Codex behavior. Explore and inspect as needed, but do not mutate files, config, app-server state, plugins, skills, MCP/apps, memories, repo-ci, model-router, tool-router, or any other Codex state until the user accepts the proposed plan. Do not attempt writes to prove they are blocked. When ready, emit exactly one complete `<proposed_plan>` block describing the config changes, validation, refresh/reload steps, and rollback considerations.",
            target_cwd.display(),
            workspace.display(),
            codex_home.display()
        ),
    ];

    push_shared_context(&mut sections);
    sections.join("\n\n")
}

fn build_codex_config_edit_context(
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> String {
    let mut sections = vec![format!(
        "# Codex Config Edit Mode\n\nTarget workspace/repository, read-only while applying: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory in edit mode: `{}`\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nApply only the approved Codex configuration plan. Write only under the Codex config directory or `/tmp`, do not modify the target workspace/repository, then validate and reload or describe any required restart. Do not emit a new `<proposed_plan>` for an apply turn.",
        target_cwd.display(),
        workspace.display(),
        codex_home.display()
    )];

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

    lines.push("Bare /codex enters Codex config planning mode, matching /plan interaction semantics while aiming the plan at Codex configuration. /codex <request> switches to that same planning mode and submits the request. The planning turn may write only under /tmp; accepting the plan starts a Codex config edit turn that may write the Codex config directory and /tmp, but not the target workspace.".to_string());

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
