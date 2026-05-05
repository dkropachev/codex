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
use tokio::process::Command;
use tokio::time::timeout;

const CODEX_GUIDE: &str = include_str!("../../../tui/codex_guide.md");
const HELP_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_HELP_SECTION_CHARS: usize = 4_000;
const MAX_RUNTIME_CONTEXT_CHARS: usize = 96_000;

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

pub(crate) async fn codex_config_intent_input(
    intent: String,
    caller_context: Option<String>,
    target_cwd: &Path,
    workspace: &Path,
) -> UserInput {
    let intent = intent.trim();
    let runtime_context = codex_config_runtime_context(caller_context, target_cwd, workspace).await;
    UserInput::Text {
        text: codex_config_prompt(intent, &runtime_context),
        text_elements: Vec::new(),
    }
}

fn codex_config_prompt(intent: &str, runtime_context: &str) -> String {
    format!(
        "Codex configuration request.\n\nUser request:\n```text\n{intent}\n```\n\nRuntime Codex context:\n<runtime_context>\n{runtime_context}\n</runtime_context>\n\nYou are configuring Codex itself. Interpret the request as a generic Codex configuration or maintenance intent. Identify the relevant Codex module, slash command, CLI command, app-server API, routed tool, or config surface from the request and runtime context, then try to complete the request end to end.\n\nRules:\n- Do not use hardcoded assumptions for pronouns or vague targets. If the target is ambiguous, ask a clarifying question before changing config.\n- Do not write to the target workspace. Use the writable scratch workspace for scripts, generated files, temporary edits, and captured output.\n- Treat runtime context as a starting point, not as a static authority. Inspect current Codex surfaces when needed using available tools, more CLI help, the embedded guide, or source search in the read-only target workspace.\n- Prefer structured APIs and routed tools when they exist. Use CLI commands when they are the maintained public interface or no structured API is available.\n- Do not edit config files manually when a supported config API, tool, or CLI command can do the same write.\n- If a request combines several config changes, preserve all of them unless they conflict; ask before dropping any part.\n- After writing config, reload, relearn, or refresh through the documented flow when the target requires it.\n- If the request is unsupported or unsafe, explain what is missing and leave config unchanged."
    )
}

async fn codex_config_runtime_context(
    caller_context: Option<String>,
    target_cwd: &Path,
    workspace: &Path,
) -> String {
    let mut sections = vec![format!(
        "Target workspace/repository, read-only for this request: `{}`\nWritable scratch workspace for scripts, generated files, and captured output: `{}`",
        target_cwd.display(),
        workspace.display()
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
    fn prompt_uses_runtime_context_and_asks_for_ambiguous_targets() {
        let prompt = codex_config_prompt(
            "disable it",
            "Slash commands generated from registry:\n- /repo-ci: configure repo CI",
        );

        assert!(prompt.contains("disable it"));
        assert!(prompt.contains("/repo-ci: configure repo CI"));
        assert!(prompt.contains("ask a clarifying question"));
        assert!(prompt.contains("Do not write to the target workspace"));
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
