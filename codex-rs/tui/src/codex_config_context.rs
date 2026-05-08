use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SandboxPolicy;

use crate::codex_guide::codex_guide_markdown;
use crate::slash_command::built_in_slash_commands;

pub(crate) const CODEX_CONFIG_DONE_MARKER: &str = "<codex_config_done>";

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

pub(crate) fn codex_config_sandbox_policy() -> SandboxPolicy {
    SandboxPolicy::WorkspaceWrite {
        writable_roots: Vec::new(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    }
}

pub(crate) fn codex_config_permission_profile() -> PermissionProfile {
    PermissionProfile::from_legacy_sandbox_policy(&codex_config_sandbox_policy())
}

pub(crate) fn codex_config_mask(target_cwd: &Path) -> CollaborationModeMask {
    let workspace = codex_config_workspace_for_cwd(target_cwd);
    CollaborationModeMask {
        name: ModeKind::Codex.display_name().to_string(),
        mode: Some(ModeKind::Codex),
        model: None,
        reasoning_effort: Some(Some(ReasoningEffort::Medium)),
        developer_instructions: Some(Some(build_codex_config_context(target_cwd, &workspace))),
    }
}

pub(crate) fn build_codex_config_context(target_cwd: &Path, workspace: &Path) -> String {
    let mut sections = vec![format!(
        "# Codex Config Mode\n\nTarget workspace/repository, read-only for this mode: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\n\nYou are configuring Codex itself. Use the guide, slash-command registry, CLI help, tools, app-server APIs, and source inspection to satisfy Codex configuration requests. Do not write to the target workspace. If you need files or scripts, create them under the scratch workspace. When the configuration task is done and the TUI should ask whether to leave Codex config mode, put `{CODEX_CONFIG_DONE_MARKER}` on its own line in the final answer.",
        target_cwd.display(),
        workspace.display()
    )];

    sections.push(format!(
        "Codex guide:\n```markdown\n{}\n```",
        codex_guide_markdown()
    ));

    sections.push(format!(
        "CLI help generated from `codex --help`:\n```text\n{}\n```",
        codex_help()
    ));

    sections.push(slash_command_context());

    sections.join("\n\n")
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

    lines.push("Bare /codex enters Codex config mode; /codex <request> submits the same AI-backed configuration context as a one-shot request.".to_string());

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
