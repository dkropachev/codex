use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio::process::Command;
use tokio::time::timeout;

const CODEX_GUIDE: &str = include_str!("../../../tui/codex_guide.md");
const PLAN_MODE_GUIDE: &str =
    include_str!("../../../collaboration-mode-templates/templates/plan.md");
const HELP_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_HELP_SECTION_CHARS: usize = 4_000;
const MAX_RUNTIME_CONTEXT_CHARS: usize = 96_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexConfigIntentMode {
    Plan,
    Edit,
}

pub(crate) struct CodexConfigIntentTurn {
    pub(crate) mode: CodexConfigIntentMode,
    pub(crate) input: UserInput,
    pub(crate) developer_instructions: String,
}

const CONFIG_EDIT_VERBS: &[&str] = &["apply", "implement"];

const CONFIG_EDIT_PHRASES: &[&str] = &[
    "accepted plan",
    "approved plan",
    "apply the plan",
    "implement the plan",
];

pub(crate) fn codex_config_workspace_for_target(target_cwd: &Path) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    target_cwd.to_string_lossy().hash(&mut hasher);
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

pub(crate) fn codex_config_plan_sandbox_policy() -> SandboxPolicy {
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

pub(crate) fn codex_config_edit_sandbox_policy(codex_home: &AbsolutePathBuf) -> SandboxPolicy {
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

pub(crate) async fn codex_config_intent_turn(
    intent: String,
    caller_context: Option<String>,
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> CodexConfigIntentTurn {
    let intent = intent.trim().to_string();
    let mode = classify_codex_config_intent(&intent);
    let runtime_context =
        codex_config_runtime_context(caller_context, target_cwd, workspace, codex_home).await;
    let developer_instructions = match mode {
        CodexConfigIntentMode::Plan => codex_config_plan_developer_instructions(&runtime_context),
        CodexConfigIntentMode::Edit => codex_config_edit_developer_instructions(&runtime_context),
    };

    CodexConfigIntentTurn {
        mode,
        input: UserInput::Text {
            text: intent,
            text_elements: Vec::new(),
        },
        developer_instructions,
    }
}

fn classify_codex_config_intent(request: &str) -> CodexConfigIntentMode {
    let normalized = normalize_request(request);
    let words = normalized.split_whitespace().collect::<Vec<_>>();

    if contains_any_phrase(&normalized, CONFIG_EDIT_PHRASES)
        || words.iter().any(|word| CONFIG_EDIT_VERBS.contains(word))
    {
        CodexConfigIntentMode::Edit
    } else {
        CodexConfigIntentMode::Plan
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

fn codex_config_plan_developer_instructions(runtime_context: &str) -> String {
    format!(
        "{PLAN_MODE_GUIDE}\n\n# Codex Config Planning Mode\n\nThe user-authored request is delivered as the visible user message for this turn. The runtime Codex context below is internal model context; do not print it, quote it wholesale, or treat it as user-authored content.\n\nRuntime Codex context:\n<runtime_context>\n{runtime_context}\n</runtime_context>\n\nWork exactly like Plan Mode, but plan changes to Codex configuration and local Codex behavior. Explore and inspect as needed, but do not mutate files, config, app-server state, plugins, skills, MCP/apps, memories, repo-ci, model-router, tool-router, or any other Codex state until a later apply turn. Do not attempt writes to prove they are blocked. When ready, emit exactly one complete `<proposed_plan>` block describing the config changes, validation, refresh/reload steps, and rollback considerations."
    )
}

fn codex_config_edit_developer_instructions(runtime_context: &str) -> String {
    format!(
        "# Codex Config Edit Mode\n\nThe user-authored request is delivered as the visible user message for this turn. The runtime Codex context below is internal model context; do not print it, quote it wholesale, or treat it as user-authored content.\n\nRuntime Codex context:\n<runtime_context>\n{runtime_context}\n</runtime_context>\n\nApply only the approved Codex configuration plan. Write only under the Codex config directory or `/tmp`, do not modify the target workspace/repository, then validate and reload or describe any required restart. Do not emit a new `<proposed_plan>` for an apply turn."
    )
}

async fn codex_config_runtime_context(
    caller_context: Option<String>,
    target_cwd: &Path,
    workspace: &Path,
    codex_home: &AbsolutePathBuf,
) -> String {
    let mut sections = vec![format!(
        "Target workspace/repository, read-only for this request: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`\nCodex config directory for approved edit turns: `{}`",
        target_cwd.display(),
        workspace.display(),
        codex_home.display()
    )];
    if let Some(context) = caller_context
        .map(|context| context.trim().to_string())
        .filter(|context| !context.is_empty())
    {
        sections.push(format!(
            "Caller-provided runtime context:\n{}",
            truncate_chars(&context, MAX_RUNTIME_CONTEXT_CHARS)
        ));
    }

    sections.push(format!("Codex guide:\n```markdown\n{CODEX_GUIDE}\n```"));
    sections.push(collect_codex_cli_help_context().await);

    truncate_chars(&sections.join("\n\n"), MAX_RUNTIME_CONTEXT_CHARS)
}

async fn collect_codex_cli_help_context() -> String {
    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe() {
        candidates.push(current_exe);
    }
    candidates.push(PathBuf::from("codex"));

    for candidate in candidates {
        let Some(top_level_help) = command_help(&candidate, &["--help"]).await else {
            continue;
        };
        if !looks_like_codex_cli_help(&top_level_help) {
            continue;
        }

        let mut sections = vec![format!(
            "$ codex --help\n{}",
            truncate_chars(&top_level_help, MAX_HELP_SECTION_CHARS)
        )];
        for command in parse_top_level_commands(&top_level_help) {
            let args = [command.as_str(), "--help"];
            if let Some(help) = command_help(&candidate, &args).await {
                sections.push(format!(
                    "$ codex {command} --help\n{}",
                    truncate_chars(&help, MAX_HELP_SECTION_CHARS)
                ));
            }
        }

        return format!(
            "CLI help generated from `{}`:\n\n{}",
            candidate.display(),
            sections.join("\n\n")
        );
    }

    "CLI help unavailable from the current executable or `codex` on PATH. Run `codex --help` and command-specific `codex <command> --help` if CLI syntax is needed."
        .to_string()
}

async fn command_help(program: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = timeout(HELP_TIMEOUT, command.output()).await.ok()?.ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

fn looks_like_codex_cli_help(help: &str) -> bool {
    help.contains("Codex CLI") || help.contains("codex [OPTIONS]")
}

fn parse_top_level_commands(help: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_commands = false;
    for line in help.lines() {
        let trimmed = line.trim();
        if trimmed == "Commands:" {
            in_commands = true;
            continue;
        }
        if !in_commands {
            continue;
        }
        if trimmed == "Options:" {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        let Some(command) = trimmed.split_whitespace().next() else {
            continue;
        };
        if command
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            commands.push(command.to_string());
        }
    }
    commands
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    if truncated.len() < text.len() {
        truncated.push_str("\n[truncated]");
    }
    truncated
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn config_plan_instructions_use_runtime_context_and_plan_rules() {
        let instructions = codex_config_plan_developer_instructions(
            "Slash commands generated from registry:\n- /repo-ci: configure repo CI",
        );

        assert!(instructions.contains("/repo-ci: configure repo CI"));
        assert!(instructions.contains("Plan Mode"));
        assert!(instructions.contains("do not mutate files"));
        assert!(instructions.contains("<proposed_plan>"));
        assert!(!instructions.contains("User request:"));
    }

    #[tokio::test]
    async fn intent_turn_keeps_visible_input_separate_from_runtime_context() {
        let codex_home =
            AbsolutePathBuf::from_absolute_path("/codex-home").expect("absolute codex home");
        let turn = codex_config_intent_turn(
            "  update repo-ci defaults  ".to_string(),
            Some("Slash commands generated from registry".to_string()),
            Path::new("/target"),
            Path::new("/scratch"),
            &codex_home,
        )
        .await;

        assert_eq!(turn.mode, CodexConfigIntentMode::Plan);
        assert_eq!(
            turn.input,
            UserInput::Text {
                text: "update repo-ci defaults".to_string(),
                text_elements: Vec::new(),
            }
        );
        assert!(
            turn.developer_instructions
                .contains("Slash commands generated from registry")
        );
        assert!(!turn.developer_instructions.contains("User request:"));
    }

    #[test]
    fn classify_config_intent_plans_until_apply_request() {
        assert_eq!(
            classify_codex_config_intent("update repo-ci defaults"),
            CodexConfigIntentMode::Plan
        );
        assert_eq!(
            classify_codex_config_intent(
                "i don't want repo-ci to run cibuildwheel or integration tests at all"
            ),
            CodexConfigIntentMode::Plan
        );
        assert_eq!(
            classify_codex_config_intent("repo-ci cibuildwheel integration tests"),
            CodexConfigIntentMode::Plan
        );
        assert_eq!(
            classify_codex_config_intent("implement the plan"),
            CodexConfigIntentMode::Edit
        );
    }

    #[test]
    fn parses_top_level_clap_commands() {
        let help = "Codex CLI\n\nCommands:\n  exec          Run Codex non-interactively\n  repo-ci       Learn and run repository CI checks\n  model-router  Tune and apply model-router metrics\n\nOptions:\n  -h, --help\n";

        assert_eq!(
            parse_top_level_commands(help),
            vec!["exec", "repo-ci", "model-router"]
        );
    }
}
