use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_config::CONFIG_TOML_FILE;
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

fn codex_config_plan_sandbox_policy(codex_home: &AbsolutePathBuf) -> SandboxPolicy {
    SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![codex_home.clone()],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: false,
    }
}

pub(crate) fn codex_config_plan_permission_profile(
    codex_home: &AbsolutePathBuf,
) -> PermissionProfile {
    PermissionProfile::from_legacy_sandbox_policy(&codex_config_plan_sandbox_policy(codex_home))
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
        codex_config_plan_permission_profile(codex_home)
    }
}

#[derive(Debug)]
pub(crate) struct CodexConfigBackup {
    config_path: PathBuf,
    backup_path: PathBuf,
    original_contents: Option<Vec<u8>>,
}

impl CodexConfigBackup {
    pub(crate) fn finalize(self) -> io::Result<()> {
        let current_contents = read_optional_file(&self.config_path)?;
        if current_contents == self.original_contents {
            match std::fs::remove_file(&self.backup_path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

pub(crate) fn create_codex_config_backup(
    codex_home: &AbsolutePathBuf,
) -> io::Result<CodexConfigBackup> {
    std::fs::create_dir_all(codex_home.as_path())?;
    let config_path = codex_home.as_path().join(CONFIG_TOML_FILE);
    let original_contents = read_optional_file(&config_path)?;
    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let mut backup_path = codex_home.as_path().join(format!(
        "{CONFIG_TOML_FILE}.codex-backup-{now_millis}-{pid}.bak"
    ));
    let mut attempt = 1;
    while backup_path.exists() {
        backup_path = codex_home.as_path().join(format!(
            "{CONFIG_TOML_FILE}.codex-backup-{now_millis}-{pid}-{attempt}.bak"
        ));
        attempt += 1;
    }
    std::fs::write(&backup_path, original_contents.as_deref().unwrap_or(&[]))?;
    Ok(CodexConfigBackup {
        config_path,
        backup_path,
        original_contents,
    })
}

fn read_optional_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
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
            "# Codex Config Mode\n\nTarget workspace/repository, read-only: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory: `{}`\nEverything else is read-only unless it is an allowed tool cache under `/tmp`.\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nUse Plan Mode's exploration and `<proposed_plan>` conventions when a plan is the right answer, but this Codex-specific filesystem rule overrides Plan Mode's mutation rule: you may write only under the Codex config directory, the scratch workspace, or `/tmp`; never modify the target workspace/repository. Use scripts and local commands as needed for investigation, writing their outputs only to scratch space or `/tmp`. If you change config, validate and reload or describe any required restart. The TUI owns the config backup for this mode; do not delete backup files.",
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
        "# Codex Config Edit Mode\n\nTarget workspace/repository, read-only while applying: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nWritable Codex config directory in edit mode: `{}`\n\nThe user-authored request is delivered as the visible user message for the turn. The generated Codex guide, slash-command registry, CLI help, and runtime context are internal model context; do not print them, quote them wholesale, or treat them as user-authored content.\n\nApply only the approved Codex configuration plan. Write only under the Codex config directory, the scratch workspace, or `/tmp`; do not modify the target workspace/repository. Validate and reload or describe any required restart. Do not emit a new `<proposed_plan>` for an apply turn. The TUI owns the config backup for this mode; do not delete backup files.",
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

    lines.push("Bare /codex enters Codex config mode. /codex <request> switches to that same mode and submits the request. Codex config mode may read the target workspace, but may write only under the Codex config directory, its scratch workspace, or /tmp. It may emit a <proposed_plan> when planning is appropriate; accepting that plan starts a Codex config edit turn with the same filesystem limits.".to_string());

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
