use std::sync::Arc;

use super::PreviousSectionState;
use super::WorldStateSection;
use crate::context::ContextualUserFragment;
use crate::context::PullRequestContext;

/// Turn-scoped pull request intent made visible through built-in world state.
pub(crate) struct PullRequestContextState {
    context: Arc<PullRequestContext>,
}

impl PullRequestContextState {
    pub(crate) fn new(context: Arc<PullRequestContext>) -> Self {
        Self { context }
    }
}

impl WorldStateSection for PullRequestContextState {
    const ID: &'static str = "pull_request_context";
    type Snapshot = PullRequestContext;

    fn snapshot(&self) -> Self::Snapshot {
        self.context.as_ref().clone()
    }

    fn matches_legacy_fragment(role: &str, text: &str) -> bool {
        role == "user" && PullRequestContext::matches_text(text)
    }

    fn has_retained_fragment_matcher() -> bool {
        true
    }

    fn matches_retained_fragment(role: &str, text: &str) -> bool {
        Self::matches_legacy_fragment(role, text)
    }

    fn render_diff(
        &self,
        previous: PreviousSectionState<'_, Self::Snapshot>,
    ) -> Option<Box<dyn ContextualUserFragment>> {
        if matches!(previous, PreviousSectionState::Known(previous) if previous == self.context.as_ref())
            || matches!(previous, PreviousSectionState::Unknown)
        {
            return None;
        }

        Some(Box::new(self.context.as_ref().clone()))
    }
}

#[cfg(test)]
#[path = "pull_request_context_tests.rs"]
mod tests;
