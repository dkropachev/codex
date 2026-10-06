use codex_git_utils::PullRequestMetadata;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;

use super::*;
use crate::context::world_state::WorldState;

#[test]
fn renders_initial_context_and_only_changed_diffs() {
    let initial_context = context("Initial intent");
    let initial = world_state(Arc::clone(&initial_context));
    let initial_rendered = initial.render_full();

    assert_eq!(initial_rendered.len(), 1);
    assert_eq!(initial_rendered[0].render(), initial_context.render());
    assert!(initial.render_diff(&initial.snapshot()).is_empty());

    let changed_context = context("Changed intent");
    let changed = world_state(Arc::clone(&changed_context));
    let changed_rendered = changed.render_diff(&initial.snapshot());
    assert_eq!(changed_rendered.len(), 1);
    assert_eq!(changed_rendered[0].render(), changed_context.render());
}

#[test]
fn restores_context_only_when_the_fragment_was_not_retained() {
    let context = context("Review intent");
    let state = world_state(Arc::clone(&context));
    let snapshot = state.snapshot();
    let retained: ResponseItem = ContextualUserFragment::into(context.as_ref().clone());

    let restored = state.render_history_diff(Some(&snapshot), &[]);
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].render(), context.render());
    assert!(
        state
            .render_history_diff(Some(&snapshot), &[retained])
            .is_empty()
    );
}

fn world_state(context: Arc<PullRequestContext>) -> WorldState {
    let mut state = WorldState::default();
    state.add_section(PullRequestContextState::new(context));
    state
}

fn context(body: &str) -> Arc<PullRequestContext> {
    Arc::new(PullRequestContext::new(PullRequestMetadata {
        number: 42,
        title: "Exhaustive review".to_string(),
        body: body.to_string(),
        url: "https://github.com/openai/codex/pull/42".to_string(),
        state: "OPEN".to_string(),
        base_ref_name: "main".to_string(),
        base_ref_oid: "base-oid".to_string(),
        head_ref_oid: "head-oid".to_string(),
    }))
}
