use super::ContextualUserFragment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoCiFollowup {
    text: String,
}

impl RepoCiFollowup {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl ContextualUserFragment for RepoCiFollowup {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<repo_ci_followup>";
    const END_MARKER: &'static str = "</repo_ci_followup>";

    fn body(&self) -> String {
        format!("\n{}\n", self.text)
    }
}
