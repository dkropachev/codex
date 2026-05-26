use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;

use super::ContextualUserFragment;

const WORKFLOW_MARKDOWN_HANDOFF_TOKEN_CAP: usize = 6_000;

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
        format!(
            "\n{}\n",
            truncate_text(
                &self.markdown,
                TruncationPolicy::Tokens(WORKFLOW_MARKDOWN_HANDOFF_TOKEN_CAP),
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_handoff_body_truncates_large_reports() {
        let fragment = WorkflowMarkdownHandoff::new("workflow report ".repeat(30_000));

        let body = fragment.body();

        assert!(body.contains("tokens truncated"));
        assert!(body.len() < 30_000);
    }
}
