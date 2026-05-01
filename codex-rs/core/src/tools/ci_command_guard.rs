use crate::session::turn_context::TurnContext;
use codex_features::Feature;
use std::collections::HashSet;

pub(crate) fn redirect_for_ci_command(turn: &TurnContext, command: &str) -> Option<String> {
    let classification = classify_ci_command(turn, command)?;
    if !turn.config.active_project.is_trusted() {
        return Some(format!(
            "Blocked regular CI shell command `{}` because this project is not trusted. Ask the user to run it explicitly or mark the project trusted before using repo-ci.",
            classification.matched_command
        ));
    }
    if repo_ci_available(turn) {
        return Some(format!(
            "Blocked regular CI shell command `{}`. Use repo_ci.run instead; it owns discovery, caching, execution, and log artifacts. Brief repo-ci failures return error_output plus artifact_id, and detailed logs are available through repo_ci.result.",
            classification.matched_command
        ));
    }
    Some(format!(
        "Blocked regular CI shell command `{}` because repo-ci tools are unavailable in this session. Ask the user to run it explicitly or enable repo-ci for this trusted project.",
        classification.matched_command
    ))
}

fn repo_ci_available(turn: &TurnContext) -> bool {
    turn.tools_config.has_environment && turn.config.features.enabled(Feature::RepoCi)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CiCommandClassification {
    matched_command: String,
}

fn classify_ci_command(turn: &TurnContext, command: &str) -> Option<CiCommandClassification> {
    let registered = registered_repo_ci_commands(turn);
    classify_ci_command_with_registered(command, &registered)
}

fn classify_ci_command_with_registered(
    command: &str,
    registered: &HashSet<String>,
) -> Option<CiCommandClassification> {
    for segment in command_segments(command) {
        let normalized = normalize_command(&segment);
        if registered.contains(&normalized) || is_broad_ci_command(&segment) {
            return Some(CiCommandClassification {
                matched_command: segment.trim().to_string(),
            });
        }
    }
    None
}

fn registered_repo_ci_commands(turn: &TurnContext) -> HashSet<String> {
    let Ok(status) = codex_repo_ci::status(&turn.config.codex_home, &turn.cwd) else {
        return HashSet::new();
    };
    let Some(manifest) = status.manifest else {
        return HashSet::new();
    };
    manifest
        .prepare_steps
        .iter()
        .chain(&manifest.fast_steps)
        .chain(&manifest.full_steps)
        .map(|step| normalize_command(&step.command))
        .collect()
}

fn command_segments(command: &str) -> Vec<String> {
    command
        .replace("&&", ";")
        .replace("||", ";")
        .lines()
        .flat_map(|line| line.split(';'))
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize_command(command: &str) -> String {
    shlex::split(command)
        .map(|parts| parts.join(" "))
        .unwrap_or_else(|| command.split_whitespace().collect::<Vec<_>>().join(" "))
        .trim()
        .to_ascii_lowercase()
}

fn is_broad_ci_command(segment: &str) -> bool {
    let Some(tokens) = shlex::split(segment) else {
        return false;
    };
    is_broad_ci_tokens(&tokens)
}

fn is_broad_ci_tokens(tokens: &[String]) -> bool {
    let tokens = trim_command_prefix(tokens);
    let Some((program, rest)) = tokens.split_first() else {
        return false;
    };
    let program = program.as_str();
    if matches!(program, "bash" | "sh" | "zsh")
        && rest.len() >= 2
        && matches!(rest[0].as_str(), "-lc" | "-c")
    {
        return shlex::split(&rest[1]).is_some_and(|nested| is_broad_ci_tokens(&nested));
    }

    match program {
        "cargo" => first_arg_matches(rest, &["test", "check", "build", "clippy", "fmt"]),
        "just" => first_arg_matches(rest, &["fmt", "fix", "test", "build"]),
        "npm" => node_script_matches(rest),
        "pnpm" => node_script_matches(rest),
        "yarn" => node_script_matches(rest),
        "pytest" => true,
        "python" | "python3" => rest.len() >= 2 && rest[0] == "-m" && rest[1] == "pytest",
        "uv" => rest.len() >= 2 && rest[0] == "run" && rest[1] == "pytest",
        "make" => rest
            .first()
            .is_some_and(|arg| starts_with_any(arg, &["test", "build", "lint"])),
        "bazel" => first_arg_matches(rest, &["test", "build"]),
        _ => false,
    }
}

fn trim_command_prefix(tokens: &[String]) -> &[String] {
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if matches!(token, "time" | "env" | "command")
            || token.contains('=') && !token.starts_with('-')
        {
            index += 1;
        } else {
            break;
        }
    }
    &tokens[index..]
}

fn first_arg_matches(args: &[String], expected: &[&str]) -> bool {
    args.first()
        .is_some_and(|arg| expected.iter().any(|expected| arg == expected))
}

fn node_script_matches(args: &[String]) -> bool {
    let Some(first) = args.first() else {
        return false;
    };
    let script = if first == "run" {
        args.get(1).map(String::as_str)
    } else {
        Some(first.as_str())
    };
    script.is_some_and(|script| matches!(script, "test" | "build" | "lint"))
}

fn starts_with_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_broad_ci_commands() {
        for command in [
            "cargo test -p codex-core",
            "bash -lc 'cargo check'",
            "just fmt",
            "npm run lint",
            "pnpm test",
            "yarn build",
            "python -m pytest",
            "uv run pytest",
            "make test-unit",
            "bazel test //...",
        ] {
            assert!(
                classify_ci_command_with_registered(command, &HashSet::new()).is_some(),
                "expected `{command}` to classify as CI"
            );
        }
    }

    #[test]
    fn does_not_classify_inspection_commands() {
        for command in [
            "rg -n 'cargo test' README.md",
            "git status --short",
            "git diff --stat",
            "cargo metadata --format-version 1",
            "npm install",
        ] {
            assert_eq!(
                classify_ci_command_with_registered(command, &HashSet::new()),
                None,
                "expected `{command}` to remain allowed"
            );
        }
    }

    #[test]
    fn classifies_registered_step_commands() {
        let registered = HashSet::from([normalize_command("./scripts/verify --fast")]);

        assert!(
            classify_ci_command_with_registered("./scripts/verify --fast", &registered).is_some()
        );
    }
}
