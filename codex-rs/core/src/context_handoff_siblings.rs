use codex_history::ResponseItemEnvelope;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ResponseItem;
use codex_utils_output_truncation::approx_token_count;

const MAX_CALLS: usize = 32;
const MAX_ITEM_TOKENS: usize = 10_000;
const MAX_BATCH_TOKENS: usize = 20_000;

#[derive(Debug, PartialEq, Eq)]
enum CallKind {
    Checkpoint,
    Function,
    Custom,
}

#[derive(Debug)]
struct SampledCall {
    call_id: String,
    item_id: Option<ResponseItemId>,
    kind: CallKind,
}

#[derive(Debug, Default)]
pub(crate) struct HandoffSiblings {
    calls: Vec<SampledCall>,
    unsafe_response: bool,
    completed: bool,
}

impl HandoffSiblings {
    pub(crate) fn observe(&mut self, item: &ResponseItem) {
        match item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::CustomToolCall { call_id, .. } => {
                if self.calls.len() >= MAX_CALLS
                    || call_id.len() + item.id().map_or(0, |id| id.as_str().len())
                        > codex_utils_string::approx_bytes_for_tokens(MAX_ITEM_TOKENS)
                    || self
                        .calls
                        .iter()
                        .any(|existing| existing.call_id == *call_id)
                {
                    self.unsafe_response = true;
                } else {
                    let kind = if let ResponseItem::FunctionCall {
                        name, namespace, ..
                    } = item
                    {
                        if name == "handoff_context"
                            && namespace
                                .as_deref()
                                .is_none_or(|value| value == "functions")
                        {
                            CallKind::Checkpoint
                        } else {
                            CallKind::Function
                        }
                    } else {
                        CallKind::Custom
                    };
                    self.calls.push(SampledCall {
                        call_id: call_id.clone(),
                        item_id: item.id().cloned(),
                        kind,
                    });
                }
            }
            ResponseItem::Message { .. } | ResponseItem::Reasoning { .. } => {}
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::ConfigurationUpdate { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => self.unsafe_response = true,
        }
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }

    pub(crate) fn retain(
        self,
        history: &[ResponseItemEnvelope],
        checkpoint_call_id: &str,
    ) -> Result<Vec<ResponseItemEnvelope>, ()> {
        if self.unsafe_response || !self.completed {
            return Err(());
        }
        let handoffs = self
            .calls
            .iter()
            .filter(|call| call.kind == CallKind::Checkpoint)
            .count();
        if handoffs != 1
            || !self
                .calls
                .iter()
                .any(|call| call.kind == CallKind::Checkpoint && call.call_id == checkpoint_call_id)
        {
            return Err(());
        }
        let mut retained = Vec::new();
        let mut batch_tokens = 0;
        for SampledCall {
            call_id,
            item_id,
            kind,
        } in self.calls
        {
            let matching: Vec<_> = history
                .iter()
                .filter(|envelope| match &envelope.item {
                    ResponseItem::FunctionCall { call_id: id, .. }
                    | ResponseItem::CustomToolCall { call_id: id, .. }
                    | ResponseItem::CustomToolCallOutput { call_id: id, .. } => id == &call_id,
                    ResponseItem::FunctionCallOutput {
                        call_id: Some(id), ..
                    } => id == &call_id,
                    _ => false,
                })
                .take(3)
                .collect();
            let [recorded_call, output] = matching.as_slice() else {
                return Err(());
            };
            if recorded_call.item.id() != item_id.as_ref()
                || !matches!(
                    (&kind, &recorded_call.item, &output.item),
                    (
                        CallKind::Checkpoint | CallKind::Function,
                        ResponseItem::FunctionCall { .. },
                        ResponseItem::FunctionCallOutput { .. }
                    ) | (
                        CallKind::Custom,
                        ResponseItem::CustomToolCall { .. },
                        ResponseItem::CustomToolCallOutput { .. }
                    )
                )
            {
                return Err(());
            }
            if kind == CallKind::Checkpoint {
                continue;
            }
            for envelope in [*recorded_call, *output] {
                let json = serde_json::to_string(&envelope.item).map_err(|_| ())?;
                let tokens = approx_token_count(&json);
                batch_tokens += tokens;
                if tokens > MAX_ITEM_TOKENS || batch_tokens > MAX_BATCH_TOKENS {
                    return Err(());
                }
                retained.push(envelope.clone());
            }
        }
        Ok(retained)
    }
}

#[cfg(test)]
#[path = "context_handoff_siblings_tests.rs"]
mod tests;
