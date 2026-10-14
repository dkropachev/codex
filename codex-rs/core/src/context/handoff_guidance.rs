use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

pub(crate) struct HandoffGuidance(pub(crate) String);
pub(crate) struct HandoffInventory(pub(crate) String);

impl ContextualUserFragment for HandoffGuidance {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("context.handoff.guidance".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<context_handoff>", "</context_handoff>")
    }

    fn body(&self) -> String {
        self.0.clone()
    }
}

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
        self.0.clone()
    }
}
