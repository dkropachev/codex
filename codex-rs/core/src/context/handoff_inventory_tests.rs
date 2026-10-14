use super::HandoffInventory;
use crate::context::ContextualUserFragment;
use codex_utils_output_truncation::approx_token_count;
use pretty_assertions::assert_eq;

#[test]
fn context_handoff_caps_complete_rendered_inventory() {
    let fragment = HandoffInventory("process 1234\n".repeat(/*n*/ 1_000));
    let rendered = fragment.render();
    assert_eq!(approx_token_count(&rendered), 1_000);
    assert!(rendered.starts_with("<context_handoff_live_handles>process 1234"));
    assert!(rendered.ends_with("</context_handoff_live_handles>"));
    assert!(rendered.contains("[handles omitted]"));
}
