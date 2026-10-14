use super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

pub(crate) struct HandoffRejected;

impl ContextualUserFragment for HandoffRejected {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("context.handoff.rejected".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<context_handoff_rejected>", "</context_handoff_rejected>")
    }

    fn body(&self) -> String {
        "Checkpoint not installed: this response's tool pairs are unsupported, incomplete, or exceed retention limits (32 calls, 10000 estimated tokens per item, 20000 total). Ordinary history is preserved. Consume the existing results, including failures; do not rerun operations. Then retry handoff_context alone with an updated summary.".to_string()
    }
}
