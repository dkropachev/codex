use super::ContextualUserFragment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowMarkdownHandoff {
    markdown: String,
}

impl WorkflowMarkdownHandoff {
    pub fn new(markdown: impl Into<String>) -> Self {
        Self {
            markdown: markdown.into(),
        }
    }
}

impl ContextualUserFragment for WorkflowMarkdownHandoff {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<workflow_markdown_handoff>";
    const END_MARKER: &'static str = "</workflow_markdown_handoff>";

    fn body(&self) -> String {
        format!("\n{}\n", self.markdown)
    }
}
