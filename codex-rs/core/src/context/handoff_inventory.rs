use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

pub(crate) struct HandoffInventory(pub(crate) String);

impl ContextualUserFragment for HandoffInventory {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("context.handoff.inventory".to_string())
    }
    fn role(&self) -> &'static str {
        "developer"
    }
    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }
    fn type_markers() -> (&'static str, &'static str) {
        (
            "<context_handoff_live_handles>",
            "</context_handoff_live_handles>",
        )
    }
    fn body(&self) -> String {
        let (open, close) = Self::type_markers();
        let budget = codex_utils_string::approx_bytes_for_tokens(/*tokens*/ 1_000)
            - open.len()
            - close.len();
        codex_utils_string::truncate_middle_with_marker(&self.0, budget, "\n[handles omitted]\n")
    }
}

#[cfg(test)]
#[path = "handoff_inventory_tests.rs"]
mod tests;
