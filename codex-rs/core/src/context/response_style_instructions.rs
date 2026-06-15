use super::ContextualUserFragment;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerseResponseStyleInstructions;

impl ContextualUserFragment for TerseResponseStyleInstructions {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<response_style>", "</response_style>")
    }

    fn body(&self) -> String {
        "\nFor this turn only, keep ordinary chat, status, and final responses terse: one or two short paragraphs or compact bullets, no filler. Keep technical caveats, file paths, test results, and blocking details when they matter. Do not shorten code, patches, JSON, command output, PR descriptions, commit messages, reviews, or other generated artifacts unless explicitly asked.\n".to_string()
    }
}
