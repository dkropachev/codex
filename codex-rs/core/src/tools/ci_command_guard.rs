use crate::session::turn_context::TurnContext;

pub(crate) fn redirect_for_ci_command(_turn: &TurnContext, _command: &str) -> Option<String> {
    None
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
