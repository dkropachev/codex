use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::truncate_text;
use codex_workflows::WorkflowContext;

use super::ContextualUserFragment;

const WORKFLOW_YAML_TOKEN_CAP: usize = 4_000;
const WORKFLOW_README_TOKEN_CAP: usize = 3_000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkflowInstructions {
    context: WorkflowContext,
}

impl WorkflowInstructions {
    pub(crate) fn new(context: WorkflowContext) -> Self {
        Self { context }
    }
}

impl ContextualUserFragment for WorkflowInstructions {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<workflow>";
    const END_MARKER: &'static str = "</workflow>";

    fn body(&self) -> String {
        let mut body = format!(
            "\n<id>{}</id>\n<path>{}</path>\n",
            self.context.id,
            self.context.path.display()
        );
        if let Some(title) = &self.context.title {
            body.push_str(&format!("<title>{title}</title>\n"));
        }
        if let Some(description) = &self.context.user_description {
            body.push_str(&format!(
                "<userDescription>{description}</userDescription>\n"
            ));
        }
        body.push_str("<workflowYaml>\n");
        body.push_str(&truncate_text(
            &self.context.workflow_yaml,
            TruncationPolicy::Tokens(WORKFLOW_YAML_TOKEN_CAP),
        ));
        body.push_str("\n</workflowYaml>\n");
        if let Some(readme) = &self.context.readme {
            body.push_str("<readme>\n");
            body.push_str(&truncate_text(
                readme,
                TruncationPolicy::Tokens(WORKFLOW_README_TOKEN_CAP),
            ));
            body.push_str("\n</readme>\n");
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use codex_workflows::WorkflowContext;

    use super::*;

    #[test]
    fn workflow_context_body_truncates_large_yaml_and_readme() {
        let fragment = WorkflowInstructions::new(WorkflowContext {
            id: "reports/jira-summary".to_string(),
            title: None,
            user_description: None,
            root_path: PathBuf::from("/tmp/workflows"),
            path: PathBuf::from("/tmp/workflows/reports/jira-summary"),
            workflow_yaml: "workflow-yaml ".repeat(20_000),
            readme: Some("readme ".repeat(20_000)),
        });

        let body = fragment.body();

        assert!(body.contains("<workflowYaml>"));
        assert!(body.contains("<readme>"));
        assert!(body.contains("tokens truncated"));
        assert!(body.len() < 40_000);
    }
}
