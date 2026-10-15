//! Replacement history and persistence metadata shared by local and remote compaction.
//!
//! Assembly keeps the window prefix first and inserts refreshed ordinary context at the
//! existing user-or-summary boundary. Session history replacement still owns item IDs
//! and persistence; this module does not initiate compaction or advance its window.

use std::collections::HashSet;

use crate::compact::insert_initial_context_before_last_real_user_or_summary;
use crate::context::world_state::WorldState;
use crate::context::world_state::WorldStateSnapshot;
use crate::context::world_state::split_prefix_updates;
use crate::context_manager::updates::merge_world_state_updates;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::state::AutoCompactWindowIds;
use codex_history::ResponseItemEnvelope;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;

/// Metadata for a new compaction checkpoint, kept separate from its replacement history.
///
/// `Session::replace_compacted_history` assigns missing item IDs before constructing the persisted
/// `CompactedItem`, ensuring the live and persisted histories remain identical.
pub(crate) struct CompactedHistoryMetadata {
    pub(crate) input_goal_ids: HashSet<ResponseItemId>,
    pub(crate) message: String,
    pub(crate) window_number: u64,
    pub(crate) window_ids: AutoCompactWindowIds,
    pub(crate) compaction_response_id: Option<String>,
    pub(crate) compaction_model_hash: Option<String>,
    pub(crate) reviewer_compaction_hash: Option<String>,
}

/// Renders the window prefix and ordinary context with their comparison baseline.
pub(crate) async fn build_compaction_replacement_history(
    sess: &Session,
    step_context: &StepContext,
    world_state: &WorldState,
    compacted_history: Vec<ResponseItemEnvelope>,
) -> (Vec<ResponseItemEnvelope>, WorldStateSnapshot) {
    let (updates, snapshot) = sess
        .build_initial_context_with_world_state(step_context, world_state)
        .await;
    let (prefix, context) = split_prefix_updates(updates);
    let context = merge_world_state_updates(context);
    (
        assemble_compaction_history(compacted_history, prefix, context),
        snapshot,
    )
}

fn assemble_compaction_history(
    compacted_history: Vec<ResponseItemEnvelope>,
    prefix: Vec<ResponseItem>,
    context: Vec<ResponseItem>,
) -> Vec<ResponseItemEnvelope> {
    let history = insert_initial_context_before_last_real_user_or_summary(
        compacted_history,
        context.into_iter().map(ResponseItemEnvelope::new).collect(),
    );
    prefix
        .into_iter()
        .map(ResponseItemEnvelope::new)
        .chain(history)
        .collect()
}

#[cfg(test)]
#[path = "compaction_checkpoint_tests.rs"]
mod tests;
