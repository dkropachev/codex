use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

use super::ContextualUserFragment;
use super::MAX_MULTI_AGENT_ROLE_BODY_TOKENS;
use super::MultiAgentRoleInstructions;

#[test]
fn role_instructions_bound_model_visible_items() {
    let oversized = "configured role guidance ".repeat(MAX_MULTI_AGENT_ROLE_BODY_TOKENS);

    for instructions in [
        MultiAgentRoleInstructions::unmarked(oversized.clone()),
        MultiAgentRoleInstructions::catalog(oversized),
    ] {
        let rendered = instructions.render();
        assert!(approx_token_count(&rendered) <= 10_000);
        assert!(rendered.contains("tokens truncated"));
    }
}

#[test]
fn role_instructions_preserve_bounded_text() {
    let text = "Use investigators for code discovery.";

    assert_eq!(MultiAgentRoleInstructions::unmarked(text).body(), text);
    assert_eq!(MultiAgentRoleInstructions::catalog(text).body(), text);
}
