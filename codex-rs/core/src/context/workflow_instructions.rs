use codex_workflows::WorkflowContext;

use super::ContextualUserFragment;

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
        body.push_str(&self.context.workflow_yaml);
        body.push_str("\n</workflowYaml>\n");
        if let Some(readme) = &self.context.readme {
            body.push_str("<readme>\n");
            body.push_str(readme);
            body.push_str("\n</readme>\n");
        }
        body
    }
}
