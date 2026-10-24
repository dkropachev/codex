//! Installs complete or prepared-prefix compacted histories and their durable checkpoints.

use super::*;

/// Normal appends keep this generation; compaction and rollback start a new one.
#[derive(Clone, Copy)]
pub(crate) struct CompactionPrefix {
    pub(crate) generation: u64,
    pub(crate) len: usize,
    pub(crate) auth_owner_generation: Option<u64>,
}

pub(crate) enum CompactedHistoryInstallation<'a> {
    Complete(CompactedHistoryMetadata),
    Prefix {
        prefix: CompactionPrefix,
        prefix_context: Vec<ResponseItem>,
        initial_context: Vec<ResponseItemEnvelope>,
        model_info: &'a ModelInfo,
        compaction_response_id: String,
        reviewer_compaction_hash: Option<String>,
        turn_context: &'a TurnContext,
        started_item: Option<&'a TurnItem>,
    },
}

impl From<CompactedHistoryMetadata> for CompactedHistoryInstallation<'_> {
    fn from(metadata: CompactedHistoryMetadata) -> Self {
        Self::Complete(metadata)
    }
}

impl Session {
    pub(crate) async fn replace_compacted_history<'a>(
        &self,
        mut items: Vec<ResponseItemEnvelope>,
        reference_context_item: TurnContextItem,
        mut world_state_baseline: WorldStateSnapshot,
        installation: impl Into<CompactedHistoryInstallation<'a>>,
    ) -> bool {
        let installation = installation.into();
        let compaction_model_hash = match &installation {
            CompactedHistoryInstallation::Complete(metadata) => &metadata.compaction_model_hash,
            CompactedHistoryInstallation::Prefix { model_info, .. } => &model_info.comp_hash,
        };
        let mcp_revision = self
            .services
            .executed_tool_calls
            .mcp_attribution_checkpoint(/*force*/ true)
            .and_then(|(attribution, revision)| {
                items.last_mut().map(|envelope| {
                    envelope.metadata.get_or_insert_default().mcp_attribution = Some(attribution);
                    revision
                })
            });
        if let Some(checkpoint) = items.iter_mut().rev().find(|envelope| {
            matches!(
                envelope.item,
                ResponseItem::Compaction { .. } | ResponseItem::ContextCompaction { .. }
            )
        }) {
            checkpoint
                .metadata
                .get_or_insert_default()
                .compaction_model_hash = compaction_model_hash.clone();
        }
        // Wait for accepted updates to finish persisting, then keep later updates from
        // overtaking the current settings snapshot while its checkpoint is written.
        let _settings_guard = thread_settings::acquire_persistence_lock(self).await;
        let (compacted_item, started_item) = {
            let mut state = self.state.lock().await;
            let complete_replacement =
                matches!(&installation, CompactedHistoryInstallation::Complete(_));
            let (metadata, started_item) = match installation {
                CompactedHistoryInstallation::Complete(metadata) => (metadata, None),
                CompactedHistoryInstallation::Prefix {
                    prefix,
                    prefix_context,
                    initial_context,
                    model_info,
                    compaction_response_id,
                    reviewer_compaction_hash,
                    turn_context,
                    started_item,
                } => {
                    // Validate immediately before slicing and replacing the captured prefix.
                    if state.history.history_version() != prefix.generation
                        || state.history.annotated_items().len() < prefix.len
                        || self.services.model_client.auth_owner_generation()
                            != prefix.auth_owner_generation
                    {
                        return false;
                    }
                    let current = state.history.annotated_items();
                    let suffix = &current[prefix.len..];
                    if suffix.iter().any(|envelope| {
                        crate::compact_retained_tool_outputs::is_retained_tool_output(envelope)
                            || compact::is_real_user_or_agent_message(&envelope.item)
                    }) {
                        items.extend(initial_context);
                    } else {
                        items = compact::insert_initial_context_before_last_real_user_or_summary(
                            items,
                            initial_context,
                        );
                    }
                    items.splice(
                        0..0,
                        prefix_context.into_iter().map(ResponseItemEnvelope::new),
                    );
                    items.extend_from_slice(suffix);
                    // The unchanged suffix already contains updates through this baseline.
                    if let Some(checkpoint) = state.history.world_state_checkpoint() {
                        world_state_baseline = WorldStateSnapshot::from(&checkpoint.state);
                    }
                    // Prefix replacement already retains all edits in the live suffix.
                    let input_goal_ids = Default::default();
                    let (window_number, window_ids) = state.advance_auto_compact_window();
                    (
                        CompactedHistoryMetadata {
                            input_goal_ids,
                            message: String::new(),
                            window_number,
                            window_ids,
                            compaction_response_id: Some(compaction_response_id),
                            compaction_model_hash: model_info.comp_hash.clone(),
                            reviewer_compaction_hash,
                        },
                        started_item.map(|item| (turn_context, item)),
                    )
                }
            };
            for envelope in &mut items {
                Self::assign_missing_response_item_id(&mut envelope.item);
            }
            // A new compaction window can introduce a fresh model-instruction prefix.
            // Split it after IDs are present, preserving envelope attribution on every part.
            items = items
                .into_iter()
                .flat_map(|envelope| {
                    crate::context::bound_model_instruction_items(vec![envelope.item])
                        .into_iter()
                        .map(move |item| ResponseItemEnvelope {
                            item,
                            metadata: envelope.metadata.clone(),
                        })
                })
                .collect();
            // Goal edits are published outside the running task. Keep edits accepted after
            // the compaction input snapshot, in their original order, after its replacement.
            if complete_replacement {
                let replacement_goal_ids = crate::context::UserGoalUpdate::message_ids(
                    items.iter().map(|envelope| &envelope.item),
                );
                items.extend(
                    state
                        .history
                        .annotated_items()
                        .iter()
                        .filter(|envelope| {
                            crate::context::UserGoalUpdate::message_text(&envelope.item).is_some()
                                && envelope.item.id().is_some_and(|id| {
                                    !metadata.input_goal_ids.contains(id)
                                        && !replacement_goal_ids.contains(id)
                                })
                        })
                        .cloned(),
                );
            }
            let replacement_history = items.clone();
            state.replace_annotated_history(
                items,
                Some(reference_context_item.clone()),
                HistoryReplacement::Compaction {
                    reviewer_compaction_hash: metadata.reviewer_compaction_hash,
                },
            );
            state.reasoning_effort_pin = ReasoningEffortPin::Compacted;
            state
                .history
                .set_world_state_baseline(world_state_baseline.clone());
            let compacted_item = CompactedItem {
                message: metadata.message,
                replacement_history: Some(replacement_history),
                guardian_history: state.history.guardian_history_checkpoint(),
                retained_context: Some(state.history.retained_context().clone()),
                mcp_resource_origins: self.services.mcp_runtime.resource_origin_checkpoint(),
                window_number: Some(metadata.window_number),
                first_window_id: Some(metadata.window_ids.first_window_id.to_string()),
                previous_window_id: metadata
                    .window_ids
                    .previous_window_id
                    .map(|id| id.to_string()),
                window_id: Some(metadata.window_ids.window_id.to_string()),
                compaction_response_id: metadata.compaction_response_id,
                latest_token_usage_record: state.latest_token_usage_record.clone(),
                resume_metadata: Some(CompactionResumeMetadata {
                    multi_agent_version: self.multi_agent_version(),
                    last_started_turn_id: state.last_started_turn_id.clone(),
                    turn_attribution: state.turn_attribution.clone(),
                    previous_turn_settings: state.previous_turn_settings(),
                }),
            };
            (compacted_item, started_item)
        };

        if let Some((turn_context, item)) = started_item {
            // The prefix is now known to be valid; announce the install before checkpoint I/O.
            self.emit_turn_item_started(turn_context, item).await;
        }
        let world_state_item = WorldStateItem::full(world_state_baseline.into_object());
        let rollout_items = [
            RolloutItem::Compacted(Box::new(compacted_item)),
            RolloutItem::WorldState(world_state_item),
            RolloutItem::TurnContext(reference_context_item),
            // The frozen turn context must not override current settings in persisted metadata.
            RolloutItem::EventMsg(thread_settings::applied_event(self).await),
        ];
        if self.persist_rollout_items(&rollout_items).await
            && let Some(revision) = mcp_revision
        {
            self.services
                .executed_tool_calls
                .mark_mcp_attribution_persisted(revision);
        }
        {
            let mut state = self.state.lock().await;
            state.queue_pending_session_start_source(codex_hooks::SessionStartSource::Compact);
        }
        true
    }
}
