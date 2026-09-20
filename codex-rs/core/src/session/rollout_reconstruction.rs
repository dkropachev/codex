use super::*;
use crate::context::world_state::WorldStateSnapshot;
use crate::context_manager::is_user_turn_boundary;
use codex_protocol::protocol::SessionContextWindow;
use uuid::Uuid;

// Return value of `Session::reconstruct_history_from_rollout`, bundling the rebuilt history with
// the resume/fork hydration metadata derived from the same replay.
#[derive(Debug)]
pub(super) struct RolloutReconstruction {
    pub(super) history: Vec<ResponseItem>,
    pub(super) previous_turn_settings: Option<PreviousTurnSettings>,
    pub(super) reference_context_item: Option<TurnContextItem>,
    pub(super) world_state_baseline: Option<WorldStateSnapshot>,
    pub(super) window_number: u64,
    pub(super) first_window_id: Option<Uuid>,
    pub(super) previous_window_id: Option<Uuid>,
    pub(super) window_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy)]
struct ReconstructedWindow {
    number: u64,
    first_id: Option<Uuid>,
    previous_id: Option<Uuid>,
    id: Option<Uuid>,
}

#[derive(Debug, Default)]
enum TurnReferenceContextItem {
    /// No `TurnContextItem` has been seen for this replay span yet.
    ///
    /// This differs from `Cleared`: `NeverSet` means there is no evidence this turn ever
    /// established a baseline, while `Cleared` means a baseline existed and a later compaction
    /// invalidated it. Only the latter must emit an explicit clearing segment for resume/fork
    /// hydration.
    #[default]
    NeverSet,
    /// A previously established baseline was invalidated by later compaction.
    Cleared,
    /// The latest baseline established by this replay span.
    Latest(Box<TurnContextItem>),
}

#[derive(Debug, Default)]
struct ActiveReplaySegment<'a> {
    turn_id: Option<String>,
    counts_as_user_turn: bool,
    previous_turn_settings: Option<PreviousTurnSettings>,
    reference_context_item: TurnReferenceContextItem,
    world_state_replay: Vec<&'a RolloutItem>,
    base_replacement_history: Option<&'a [ResponseItem]>,
    base_replacement_suffix: Option<&'a [RolloutItem]>,
    window: Option<ReconstructedWindow>,
}

#[derive(Default)]
struct ForwardReplayTurnTracker {
    model_visible_turns: Vec<bool>,
    active_turn: Option<usize>,
    review_boundaries: std::collections::HashSet<String>,
}

impl ForwardReplayTurnTracker {
    fn start_turn(&mut self) {
        self.active_turn = None;
    }

    fn note_turn_boundary(&mut self, model_visible: bool) {
        if let Some(index) = self.active_turn {
            self.model_visible_turns[index] |= model_visible;
        } else {
            let index = self.model_visible_turns.len();
            self.model_visible_turns.push(model_visible);
            self.active_turn = Some(index);
        }
    }

    fn finish_turn(&mut self) {
        self.active_turn = None;
    }

    fn rollback(&mut self, num_turns: u32) -> u32 {
        let num_turns = usize::try_from(num_turns).unwrap_or(usize::MAX);
        let start = self.model_visible_turns.len().saturating_sub(num_turns);
        let visible_turns = self.model_visible_turns[start..]
            .iter()
            .filter(|visible| **visible)
            .count()
            .saturating_add(num_turns.saturating_sub(self.model_visible_turns.len()));
        self.model_visible_turns.truncate(start);
        self.active_turn = None;
        u32::try_from(visible_turns).unwrap_or(u32::MAX)
    }

    fn observe(&mut self, item: &RolloutItem) -> Option<u32> {
        match item {
            RolloutItem::ResponseItem(response_item)
                if super::review_handoff::is_legacy_review_user_message(response_item) =>
            {
                self.note_turn_boundary(/*model_visible*/ true);
            }
            RolloutItem::ResponseItem(response_item) if is_user_turn_boundary(response_item) => {
                self.note_turn_boundary(/*model_visible*/ true);
            }
            RolloutItem::InterAgentCommunication(_) => {
                self.note_turn_boundary(/*model_visible*/ true);
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(_)) => self.start_turn(),
            RolloutItem::EventMsg(EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_)) => {
                self.finish_turn();
            }
            RolloutItem::EventMsg(EventMsg::UserMessage(_)) => {
                self.note_turn_boundary(/*model_visible*/ false);
            }
            RolloutItem::EventMsg(EventMsg::EnteredReviewMode(entered)) => {
                self.note_review_turn(entered.item_id.as_deref().or(entered.turn_id.as_deref()));
            }
            RolloutItem::EventMsg(EventMsg::ItemCompleted(completed)) => match &completed.item {
                codex_protocol::items::TurnItem::EnteredReviewMode(entered) => {
                    self.note_review_turn(Some(&entered.id));
                }
                codex_protocol::items::TurnItem::ExitedReviewMode(_) => self.finish_turn(),
                _ => {}
            },
            RolloutItem::EventMsg(EventMsg::ExitedReviewMode(_)) => self.finish_turn(),
            RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                return Some(self.rollback(rollback.num_turns));
            }
            RolloutItem::ResponseItem(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::TurnContext(_)
            | RolloutItem::WorldState(_)
            | RolloutItem::SessionMeta(_)
            | RolloutItem::InterAgentCommunicationMetadata { .. }
            | RolloutItem::EventMsg(_) => {}
        }
        None
    }

    fn note_review_turn(&mut self, boundary: Option<&str>) {
        if boundary.is_none_or(|boundary| self.review_boundaries.insert(boundary.to_string())) {
            self.note_turn_boundary(/*model_visible*/ false);
        }
    }
}

fn turn_ids_are_compatible(active_turn_id: Option<&str>, item_turn_id: Option<&str>) -> bool {
    active_turn_id
        .is_none_or(|turn_id| item_turn_id.is_none_or(|item_turn_id| item_turn_id == turn_id))
}

fn review_turn_boundary(item: &RolloutItem) -> Option<Option<&str>> {
    match item {
        RolloutItem::EventMsg(EventMsg::EnteredReviewMode(entered)) => {
            Some(entered.item_id.as_deref().or(entered.turn_id.as_deref()))
        }
        RolloutItem::EventMsg(EventMsg::ItemCompleted(completed)) => match &completed.item {
            codex_protocol::items::TurnItem::EnteredReviewMode(entered) => {
                Some(Some(entered.id.as_str()))
            }
            _ => None,
        },
        _ => None,
    }
}

fn finalize_active_segment<'a>(
    active_segment: ActiveReplaySegment<'a>,
    base_replacement_history: &mut Option<&'a [ResponseItem]>,
    rollout_suffix: &mut &'a [RolloutItem],
    previous_turn_settings: &mut Option<PreviousTurnSettings>,
    reference_context_item: &mut TurnReferenceContextItem,
    world_state_replay: &mut Vec<&'a RolloutItem>,
    window: &mut Option<ReconstructedWindow>,
    pending_rollback_turns: &mut usize,
) {
    // Thread rollback drops the newest surviving real user-message boundaries. In replay, that
    // means skipping the next finalized segments that contain a non-contextual
    // `EventMsg::UserMessage`.
    if *pending_rollback_turns > 0 {
        if active_segment.counts_as_user_turn {
            *pending_rollback_turns -= 1;
        }
        return;
    }

    world_state_replay.extend(active_segment.world_state_replay);

    // A surviving replacement-history checkpoint is a complete history base. Once we
    // know the newest surviving one, older rollout items do not affect rebuilt history.
    if base_replacement_history.is_none()
        && let Some(segment_base_replacement_history) = active_segment.base_replacement_history
    {
        *base_replacement_history = Some(segment_base_replacement_history);
        if let Some(segment_suffix) = active_segment.base_replacement_suffix {
            *rollout_suffix = segment_suffix;
        }
    }

    if window.is_none() {
        *window = active_segment.window;
    }

    // `previous_turn_settings` come from the newest surviving user turn that established them.
    if previous_turn_settings.is_none() && active_segment.counts_as_user_turn {
        *previous_turn_settings = active_segment.previous_turn_settings;
    }

    // `reference_context_item` comes from the newest surviving user turn baseline, or
    // from a surviving compaction that explicitly cleared that baseline.
    if matches!(reference_context_item, TurnReferenceContextItem::NeverSet)
        && (active_segment.counts_as_user_turn
            || matches!(
                active_segment.reference_context_item,
                TurnReferenceContextItem::Cleared
            ))
    {
        *reference_context_item = active_segment.reference_context_item;
    }
}

impl Session {
    pub(super) async fn reconstruct_history_from_rollout(
        &self,
        turn_context: &TurnContext,
        rollout_items: &[RolloutItem],
    ) -> RolloutReconstruction {
        // Replay metadata should already match the shape of the future lazy reverse loader, even
        // while history materialization still uses an eager bridge. Scan newest-to-oldest,
        // stopping once a surviving replacement-history checkpoint and the required resume metadata
        // are both known; then replay only the buffered surviving tail forward to preserve exact
        // history semantics.
        let has_legacy_compaction_without_window_number =
            rollout_items.iter().any(|item| {
                matches!(item, RolloutItem::Compacted(compacted) if compacted.window_number.is_none())
            });
        let initial_window = if has_legacy_compaction_without_window_number {
            None
        } else {
            rollout_items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(session_meta) => session_meta
                    .meta
                    .context_window
                    .as_ref()
                    .and_then(reconstructed_window_from_session_context_window),
                _ => None,
            })
        };
        let mut base_replacement_history: Option<&[ResponseItem]> = None;
        let mut previous_turn_settings = None;
        let mut reference_context_item = TurnReferenceContextItem::NeverSet;
        let mut world_state_replay = Vec::new();
        let mut window = None;
        // Rollback is "drop the newest N user turns". While scanning in reverse, that becomes
        // "skip the next N user-turn segments we finalize".
        let mut pending_rollback_turns = 0usize;
        // Borrowed suffix of rollout items newer than the newest surviving replacement-history
        // checkpoint. If no such checkpoint exists, this remains the full rollout.
        let mut rollout_suffix = rollout_items;
        // Reverse replay accumulates rollout items into the newest in-progress turn segment until
        // we hit its matching `TurnStarted`, at which point the segment can be finalized.
        let mut active_segment: Option<ActiveReplaySegment<'_>> = None;
        let mut seen_review_boundaries = std::collections::HashSet::new();

        for (index, item) in rollout_items.iter().enumerate().rev() {
            if let Some(boundary) = review_turn_boundary(item) {
                if boundary
                    .is_none_or(|boundary| seen_review_boundaries.insert(boundary.to_string()))
                {
                    let mut review_already_counted = false;
                    if let Some(active_segment) = active_segment.take() {
                        review_already_counted = active_segment.counts_as_user_turn;
                        finalize_active_segment(
                            active_segment,
                            &mut base_replacement_history,
                            &mut rollout_suffix,
                            &mut previous_turn_settings,
                            &mut reference_context_item,
                            &mut world_state_replay,
                            &mut window,
                            &mut pending_rollback_turns,
                        );
                    }
                    if !review_already_counted {
                        finalize_active_segment(
                            ActiveReplaySegment {
                                counts_as_user_turn: true,
                                ..Default::default()
                            },
                            &mut base_replacement_history,
                            &mut rollout_suffix,
                            &mut previous_turn_settings,
                            &mut reference_context_item,
                            &mut world_state_replay,
                            &mut window,
                            &mut pending_rollback_turns,
                        );
                    }
                }
                continue;
            }
            match item {
                RolloutItem::Compacted(compacted) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.world_state_replay.push(item);
                    if active_segment.window.is_none()
                        && let Some(window_number) = compacted.window_number
                    {
                        active_segment.window = Some(ReconstructedWindow {
                            number: window_number,
                            first_id: compacted.first_window_id.as_deref().and_then(parse_uuid_v7),
                            previous_id: compacted
                                .previous_window_id
                                .as_deref()
                                .and_then(parse_uuid_v7),
                            id: compacted.window_id.as_deref().and_then(parse_uuid_v7),
                        });
                    }
                    // Looking backward, compaction clears any older baseline unless a newer
                    // `TurnContextItem` in this same segment has already re-established it.
                    if matches!(
                        active_segment.reference_context_item,
                        TurnReferenceContextItem::NeverSet
                    ) {
                        active_segment.reference_context_item = TurnReferenceContextItem::Cleared;
                    }
                    if active_segment.base_replacement_history.is_none()
                        && let Some(replacement_history) = &compacted.replacement_history
                    {
                        active_segment.base_replacement_history = Some(replacement_history);
                        active_segment.base_replacement_suffix = Some(&rollout_items[index + 1..]);
                    }
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    pending_rollback_turns = pending_rollback_turns
                        .saturating_add(usize::try_from(rollback.num_turns).unwrap_or(usize::MAX));
                }
                RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // Reverse replay often sees `TurnComplete` before any turn-scoped metadata.
                    // Capture the turn id early so later `TurnContext` / abort items can match it.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = Some(event.turn_id.clone());
                    }
                }
                RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                    if let Some(active_segment) = active_segment.as_mut() {
                        if active_segment.turn_id.is_none()
                            && let Some(turn_id) = &event.turn_id
                        {
                            active_segment.turn_id = Some(turn_id.clone());
                        }
                    } else if let Some(turn_id) = &event.turn_id {
                        active_segment = Some(ActiveReplaySegment {
                            turn_id: Some(turn_id.clone()),
                            ..Default::default()
                        });
                    }
                }
                RolloutItem::EventMsg(EventMsg::UserMessage(_)) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn = true;
                }
                RolloutItem::TurnContext(ctx) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    // `TurnContextItem` can attach metadata to an existing segment, but only a
                    // real `UserMessage` event should make the segment count as a user turn.
                    if active_segment.turn_id.is_none() {
                        active_segment.turn_id = ctx.turn_id.clone();
                    }
                    if turn_ids_are_compatible(
                        active_segment.turn_id.as_deref(),
                        ctx.turn_id.as_deref(),
                    ) {
                        active_segment.previous_turn_settings = Some(PreviousTurnSettings {
                            model: ctx.model.clone(),
                            comp_hash: ctx.comp_hash.clone(),
                            realtime_active: ctx.realtime_active,
                        });
                        if matches!(
                            active_segment.reference_context_item,
                            TurnReferenceContextItem::NeverSet
                        ) {
                            active_segment.reference_context_item =
                                TurnReferenceContextItem::Latest(Box::new(ctx.clone()));
                        }
                    }
                }
                RolloutItem::WorldState(_) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.world_state_replay.push(item);
                }
                RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                    // `TurnStarted` is the oldest boundary of the active reverse segment.
                    if active_segment.as_ref().is_some_and(|active_segment| {
                        turn_ids_are_compatible(
                            active_segment.turn_id.as_deref(),
                            Some(event.turn_id.as_str()),
                        )
                    }) && let Some(active_segment) = active_segment.take()
                    {
                        finalize_active_segment(
                            active_segment,
                            &mut base_replacement_history,
                            &mut rollout_suffix,
                            &mut previous_turn_settings,
                            &mut reference_context_item,
                            &mut world_state_replay,
                            &mut window,
                            &mut pending_rollback_turns,
                        );
                    }
                }
                RolloutItem::ResponseItem(response_item) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn |=
                        !crate::context::ReviewHandoff::is_consumption_marker(response_item)
                            && !crate::context::ReviewHandoff::is_pending_report_marker(
                                response_item,
                            )
                            && is_user_turn_boundary(response_item);
                }
                RolloutItem::InterAgentCommunication(_) => {
                    let active_segment =
                        active_segment.get_or_insert_with(ActiveReplaySegment::default);
                    active_segment.counts_as_user_turn = true;
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::SessionMeta(_)
                | RolloutItem::InterAgentCommunicationMetadata { .. } => {}
            }

            if base_replacement_history.is_some()
                && previous_turn_settings.is_some()
                && !matches!(reference_context_item, TurnReferenceContextItem::NeverSet)
            {
                // At this point we have both eager resume metadata values and the replacement-
                // history base for the surviving tail, so older rollout items cannot affect this
                // result.
                break;
            }
        }

        if let Some(active_segment) = active_segment.take() {
            finalize_active_segment(
                active_segment,
                &mut base_replacement_history,
                &mut rollout_suffix,
                &mut previous_turn_settings,
                &mut reference_context_item,
                &mut world_state_replay,
                &mut window,
                &mut pending_rollback_turns,
            );
        }

        let fallback_window_number = u64::try_from(
            rollout_items
                .iter()
                .filter(|item| matches!(item, RolloutItem::Compacted(_)))
                .count(),
        )
        .unwrap_or(u64::MAX);

        let mut history = ContextManager::new();
        let mut saw_legacy_compaction_without_replacement_history = false;
        if let Some(base_replacement_history) = base_replacement_history {
            history.replace(
                super::review_handoff::filter_complete_review_handoff_history(
                    base_replacement_history.iter().cloned(),
                ),
            );
        }
        // Materialize exact history semantics from the replay-derived suffix. The eventual lazy
        // design should keep this same replay shape, but drive it from a resumable reverse source
        // instead of an eagerly loaded `&[RolloutItem]`.
        let mut review_handoff_filter = super::review_handoff::ReviewHandoffHistoryFilter::new();
        let mut forward_turns = ForwardReplayTurnTracker::default();
        let rollout_suffix_start = rollout_items.len().saturating_sub(rollout_suffix.len());
        for item in &rollout_items[..rollout_suffix_start] {
            forward_turns.observe(item);
        }
        for item in rollout_suffix {
            let rolled_back_visible_turns = forward_turns.observe(item);
            if !matches!(
                item,
                RolloutItem::ResponseItem(_) | RolloutItem::InterAgentCommunicationMetadata { .. }
            ) {
                review_handoff_filter.interrupt();
            }
            match item {
                RolloutItem::ResponseItem(response_item) => {
                    for response_item in review_handoff_filter.push(response_item.clone()) {
                        history.record_items(
                            std::iter::once(&response_item),
                            turn_context.model_info.truncation_policy.into(),
                        );
                    }
                }
                RolloutItem::InterAgentCommunication(communication) => {
                    let response_item = communication.to_model_input_item();
                    history.record_items(
                        std::iter::once(&response_item),
                        turn_context.model_info.truncation_policy.into(),
                    );
                }
                RolloutItem::InterAgentCommunicationMetadata { .. } => {}
                RolloutItem::Compacted(compacted) => {
                    if let Some(replacement_history) = &compacted.replacement_history {
                        // This should actually never happen, because the reverse loop above (to build rollout_suffix)
                        // should stop before any compaction that has Some replacement_history
                        history.replace(replacement_history.clone());
                    } else {
                        saw_legacy_compaction_without_replacement_history = true;
                        // Legacy rollouts without `replacement_history` should rebuild the
                        // historical TurnContext at the correct insertion point from persisted
                        // `TurnContextItem`s. These are rare enough that we currently just clear
                        // `reference_context_item`, reinject canonical context at the end of the
                        // resumed conversation, and accept the temporary out-of-distribution
                        // prompt shape.
                        // TODO(ccunningham): if we drop support for None replacement_history compaction items,
                        // we can get rid of this second loop entirely and just build `history` directly in the first loop.
                        let user_messages = compact::collect_user_messages(history.raw_items());
                        let rebuilt = compact::build_compacted_history(
                            Vec::new(),
                            &user_messages,
                            &compacted.message,
                        );
                        history.replace(rebuilt);
                    }
                }
                RolloutItem::EventMsg(EventMsg::ThreadRolledBack(rollback)) => {
                    let visible_turns = rolled_back_visible_turns
                        .unwrap_or_else(|| forward_turns.rollback(rollback.num_turns));
                    history.drop_last_n_user_turns(visible_turns);
                }
                RolloutItem::EventMsg(_)
                | RolloutItem::TurnContext(_)
                | RolloutItem::WorldState(_)
                | RolloutItem::SessionMeta(_) => {}
            }
        }

        let reference_context_item = match reference_context_item {
            TurnReferenceContextItem::NeverSet | TurnReferenceContextItem::Cleared => None,
            TurnReferenceContextItem::Latest(turn_reference_context_item) => {
                Some(*turn_reference_context_item)
            }
        };
        let reference_context_item = if saw_legacy_compaction_without_replacement_history {
            None
        } else {
            reference_context_item
        };

        // Segments and their contents were collected newest-first; replay the surviving records
        // chronologically so compaction resets and merge patches have their original meaning.
        world_state_replay.reverse();
        let mut world_state_baseline: Option<WorldStateSnapshot> = None;
        for item in world_state_replay {
            match item {
                RolloutItem::Compacted(_) => world_state_baseline = None,
                RolloutItem::WorldState(world_state) if world_state.full => {
                    world_state_baseline = match serde_json::from_value(world_state.state.clone()) {
                        Ok(snapshot) => Some(snapshot),
                        Err(err) => {
                            tracing::warn!(%err, "failed to restore world-state snapshot");
                            None
                        }
                    };
                }
                RolloutItem::WorldState(world_state) => {
                    let Some(baseline) = world_state_baseline.as_mut() else {
                        tracing::warn!("ignored world-state patch without a full snapshot");
                        continue;
                    };
                    if let Err(err) = baseline.apply_merge_patch(&world_state.state) {
                        tracing::warn!(%err, "failed to apply world-state patch");
                        world_state_baseline = None;
                    }
                }
                RolloutItem::SessionMeta(_)
                | RolloutItem::ResponseItem(_)
                | RolloutItem::InterAgentCommunication(_)
                | RolloutItem::InterAgentCommunicationMetadata { .. }
                | RolloutItem::TurnContext(_)
                | RolloutItem::EventMsg(_) => {
                    unreachable!("only world-state replay items are collected")
                }
            }
        }

        let window = window.or(initial_window).unwrap_or(ReconstructedWindow {
            number: fallback_window_number,
            first_id: None,
            previous_id: None,
            id: None,
        });
        RolloutReconstruction {
            history: history.into_raw_items(),
            previous_turn_settings,
            reference_context_item,
            world_state_baseline,
            window_number: window.number,
            first_window_id: window.first_id,
            previous_window_id: window.previous_id,
            window_id: window.id,
        }
    }
}

fn parse_uuid_v7(value: &str) -> Option<Uuid> {
    Uuid::parse_str(value)
        .ok()
        .filter(|uuid| uuid.get_version_num() == 7)
}

fn reconstructed_window_from_session_context_window(
    context_window: &SessionContextWindow,
) -> Option<ReconstructedWindow> {
    let id = parse_uuid_v7(&context_window.window_id)?;
    Some(ReconstructedWindow {
        number: 0,
        first_id: Some(id),
        previous_id: None,
        id: Some(id),
    })
}
