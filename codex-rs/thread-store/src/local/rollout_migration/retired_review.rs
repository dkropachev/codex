//! Finds historical inline-review turns that must not survive migration.
//!
//! The retired `/review` flow persisted protocol items that no longer deserialize. It could also
//! leave an immediately following interrupted child turn containing the same synthetic user
//! prompt twice. This planner recognizes that narrow historical turn shape from raw JSON and
//! returns source-record indexes to omit before typed deserialization.

use std::collections::HashSet;
use std::path::Path;

use codex_protocol::protocol::UserMessageEvent;
use serde_json::Value;
use tokio::fs::File;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;

use super::MAX_ROLLOUT_LINE_BYTES;
use super::PROJECTION_BATCH_BYTES;
use super::line_parser::is_retired_review_response;
use super::migration_error;
use crate::ThreadStoreResult;

#[derive(Default)]
pub(super) struct RetiredReviewPlan {
    retired_record_indexes: HashSet<usize>,
}

impl RetiredReviewPlan {
    pub(super) async fn build(path: &Path) -> ThreadStoreResult<(Self, u64)> {
        let file = File::open(path).await.map_err(migration_error)?;
        let mut reader = BufReader::with_capacity(PROJECTION_BATCH_BYTES as usize, file);
        let mut bytes = Vec::new();
        let mut planner = RetiredReviewPlanner::default();
        let mut record_index = 0_usize;
        let mut bytes_read = 0_u64;

        while let Some((record_bytes, parseable)) = read_record(&mut reader, &mut bytes).await? {
            bytes_read = bytes_read
                .checked_add(record_bytes)
                .ok_or_else(|| migration_error("retired review scan byte count overflow"))?;
            if parseable && let Ok(value) = serde_json::from_slice(&bytes) {
                planner.observe(record_index, &value);
            }
            record_index = record_index
                .checked_add(1)
                .ok_or_else(|| migration_error("retired review record index overflow"))?;
        }

        Ok((planner.finish(), bytes_read))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.retired_record_indexes.is_empty()
    }

    pub(super) fn contains(&self, record_index: usize) -> bool {
        self.retired_record_indexes.contains(&record_index)
    }
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum LegacyTurnStatus {
    #[default]
    Completed,
    InProgress,
    Interrupted,
}

#[derive(Default)]
struct LegacyTurn {
    id: String,
    status: LegacyTurnStatus,
    opened_explicitly: bool,
    completed_at_is_none: bool,
    saw_abort: bool,
    entered_review: bool,
    exited_review: bool,
    user_messages: Vec<UserMessageEvent>,
    record_indexes: Vec<usize>,
}

#[derive(Clone, Copy)]
enum ReviewMarker {
    Entered,
    Exited,
}

#[derive(Default)]
struct RetiredReviewPlanner {
    turns: Vec<LegacyTurn>,
    current_turn: Option<usize>,
    retired_record_indexes: HashSet<usize>,
}

impl RetiredReviewPlanner {
    fn observe(&mut self, record_index: usize, value: &Value) {
        if is_retired_review_response(value) {
            self.retired_record_indexes.insert(record_index);
        }

        if let Some((marker, turn_id)) = review_marker(value) {
            let turn_index = self.review_turn(turn_id, record_index);
            let turn = &mut self.turns[turn_index];
            match marker {
                ReviewMarker::Entered => turn.entered_review = true,
                ReviewMarker::Exited => turn.exited_review = true,
            }
            push_record(turn, record_index);
            return;
        }

        match event_type(value) {
            Some("task_started" | "turn_started") => {
                let Some(turn_id) = event_string(value, "turn_id") else {
                    return;
                };
                self.current_turn = None;
                let turn_index = self.turns.len();
                self.turns.push(LegacyTurn {
                    id: turn_id.to_string(),
                    status: LegacyTurnStatus::InProgress,
                    opened_explicitly: true,
                    record_indexes: vec![record_index],
                    ..LegacyTurn::default()
                });
                self.current_turn = Some(turn_index);
            }
            Some("task_complete" | "turn_complete") => {
                self.handle_terminal_record(
                    event_string(value, "turn_id"),
                    record_index,
                    LegacyTurnStatus::Completed,
                    /*completed_at_is_none*/ false,
                );
            }
            Some("turn_aborted") => {
                let completed_at_is_none = event_payload(value)
                    .and_then(|payload| payload.get("completed_at"))
                    .is_none_or(Value::is_null);
                self.handle_terminal_record(
                    event_string(value, "turn_id"),
                    record_index,
                    LegacyTurnStatus::Interrupted,
                    completed_at_is_none,
                );
            }
            Some("user_message") => {
                let Some(mut message) = parse_user_message(value) else {
                    return;
                };
                message.client_id = None;
                if self
                    .current_turn
                    .is_some_and(|index| !self.turns[index].opened_explicitly)
                {
                    self.current_turn = None;
                }
                let turn_index = self.ensure_current_turn(record_index);
                let turn = &mut self.turns[turn_index];
                turn.user_messages.push(message);
                push_record(turn, record_index);
            }
            _ => self.record_in_target_or_current_turn(
                event_string(value, "turn_id").or_else(|| payload_string(value, "turn_id")),
                record_index,
            ),
        }
    }

    fn finish(mut self) -> RetiredReviewPlan {
        for turns in self.turns.windows(2) {
            let marker = &turns[0];
            let child = &turns[1];
            let duplicated_review_prompt = matches!(
                child.user_messages.as_slice(),
                [first, second] if first == second
            );
            if marker.status == LegacyTurnStatus::Completed
                && marker.entered_review
                && marker.exited_review
                && child.status == LegacyTurnStatus::Interrupted
                && child.saw_abort
                && child.completed_at_is_none
                && duplicated_review_prompt
            {
                self.retired_record_indexes
                    .extend(marker.record_indexes.iter().copied());
                self.retired_record_indexes
                    .extend(child.record_indexes.iter().copied());
            }
        }
        RetiredReviewPlan {
            retired_record_indexes: self.retired_record_indexes,
        }
    }

    fn review_turn(&mut self, turn_id: Option<&str>, record_index: usize) -> usize {
        if let Some(turn_id) = turn_id {
            if let Some(index) = self.turn_index(turn_id) {
                return index;
            }
            self.current_turn = None;
            let index = self.turns.len();
            self.turns.push(LegacyTurn {
                id: turn_id.to_string(),
                record_indexes: Vec::new(),
                ..LegacyTurn::default()
            });
            self.current_turn = Some(index);
            index
        } else {
            self.ensure_current_turn(record_index)
        }
    }

    fn ensure_current_turn(&mut self, record_index: usize) -> usize {
        if let Some(index) = self.current_turn {
            return index;
        }
        let index = self.turns.len();
        self.turns.push(LegacyTurn {
            id: format!("rollout-{record_index}"),
            ..LegacyTurn::default()
        });
        self.current_turn = Some(index);
        index
    }

    fn turn_index(&self, turn_id: &str) -> Option<usize> {
        self.current_turn
            .filter(|index| self.turns[*index].id == turn_id)
            .or_else(|| self.turns.iter().position(|turn| turn.id == turn_id))
    }

    fn handle_terminal_record(
        &mut self,
        turn_id: Option<&str>,
        record_index: usize,
        status: LegacyTurnStatus,
        completed_at_is_none: bool,
    ) {
        let turn_index = turn_id
            .and_then(|turn_id| self.turn_index(turn_id))
            .or(self.current_turn);
        let Some(turn_index) = turn_index else {
            return;
        };
        let turn = &mut self.turns[turn_index];
        turn.status = status;
        turn.completed_at_is_none = completed_at_is_none;
        turn.saw_abort = status == LegacyTurnStatus::Interrupted;
        push_record(turn, record_index);
        if status == LegacyTurnStatus::Completed && self.current_turn == Some(turn_index) {
            self.current_turn = None;
        }
    }

    fn record_in_target_or_current_turn(&mut self, turn_id: Option<&str>, record_index: usize) {
        let turn_index = turn_id
            .and_then(|turn_id| self.turn_index(turn_id))
            .or(self.current_turn);
        if let Some(turn_index) = turn_index {
            push_record(&mut self.turns[turn_index], record_index);
        }
    }
}

fn push_record(turn: &mut LegacyTurn, record_index: usize) {
    if turn.record_indexes.last() != Some(&record_index) {
        turn.record_indexes.push(record_index);
    }
}

fn review_marker(value: &Value) -> Option<(ReviewMarker, Option<&str>)> {
    let marker = match event_type(value)? {
        "entered_review_mode" => ReviewMarker::Entered,
        "exited_review_mode" => ReviewMarker::Exited,
        "item_started" | "item_completed" => match event_payload(value)
            .and_then(|payload| payload.get("item"))
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
        {
            Some("enteredReviewMode" | "EnteredReviewMode") => ReviewMarker::Entered,
            Some("exitedReviewMode" | "ExitedReviewMode") => ReviewMarker::Exited,
            _ => return None,
        },
        _ => return None,
    };
    Some((marker, event_string(value, "turn_id")))
}

fn parse_user_message(value: &Value) -> Option<UserMessageEvent> {
    let mut payload = event_payload(value)?.clone();
    payload.as_object_mut()?.remove("type");
    serde_json::from_value(payload).ok()
}

fn rollout_type(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

fn event_type(value: &Value) -> Option<&str> {
    (rollout_type(value) == Some("event_msg"))
        .then(|| event_payload(value)?.get("type")?.as_str())
        .flatten()
}

fn event_payload(value: &Value) -> Option<&Value> {
    value.get("payload")
}

fn event_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    event_payload(value)?.get(field)?.as_str()
}

fn payload_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get("payload")?.get(field)?.as_str()
}

async fn read_record(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
) -> ThreadStoreResult<Option<(u64, bool)>> {
    bytes.clear();
    let mut byte_count = reader
        .take((MAX_ROLLOUT_LINE_BYTES + 1) as u64)
        .read_until(b'\n', bytes)
        .await
        .map_err(migration_error)?;
    if byte_count == 0 {
        return Ok(None);
    }
    if byte_count > MAX_ROLLOUT_LINE_BYTES {
        while bytes.last() != Some(&b'\n') {
            bytes.clear();
            let chunk_bytes = reader
                .take((MAX_ROLLOUT_LINE_BYTES + 1) as u64)
                .read_until(b'\n', bytes)
                .await
                .map_err(migration_error)?;
            if chunk_bytes == 0 {
                break;
            }
            byte_count = byte_count
                .checked_add(chunk_bytes)
                .ok_or_else(|| migration_error("retired review record byte count overflow"))?;
        }
        bytes.clear();
        return Ok(Some((byte_count as u64, false)));
    }
    Ok(Some((byte_count as u64, true)))
}
