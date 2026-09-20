use std::sync::Arc;

use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;

use crate::hook_runtime::HookRuntimeOutcome;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use crate::session::TurnInput;
use crate::session::review_handoff::ReviewHandoffLeadingInput;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecordInputOutcome {
    Continue,
    Stop,
}

#[tracing::instrument(level = "trace", skip_all)]
pub(super) async fn run_hooks_and_record_inputs_with_prefix(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &[TurnInput],
    prefix: Vec<ResponseItem>,
    review_handoff_through: Option<&str>,
    review_handoff_transaction: Option<&str>,
) -> RecordInputOutcome {
    let mut inspected = Vec::with_capacity(input.len());
    let mut blocked_input = false;
    let mut accepted_user_input = false;
    for input_item in input {
        let hook_outcome = inspect_pending_input(session, turn_context, input_item).await;
        if hook_outcome.should_stop {
            blocked_input = true;
        } else if matches!(input_item, TurnInput::UserInput { content, .. } if !content.is_empty())
        {
            accepted_user_input = true;
        }
        inspected.push((input_item.clone(), hook_outcome));
    }
    let mut atomic_input_indices = std::collections::HashSet::new();
    if (!blocked_input || accepted_user_input)
        && let (Some(through_item_id), Some(transaction_id)) =
            (review_handoff_through, review_handoff_transaction)
    {
        match record_first_accepted_input(
            session,
            turn_context,
            prefix,
            transaction_id,
            through_item_id,
            &inspected,
        )
        .await
        {
            Ok(indices) => atomic_input_indices.extend(indices),
            Err(error) => {
                tracing::warn!(%error, "failed to persist review handoff with user input");
                session
                    .send_event(
                        turn_context.as_ref(),
                        EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                            message:
                                "Failed to save the review handoff with this message. Please retry."
                                    .to_string(),
                            codex_error_info: None,
                        }),
                    )
                    .await;
                return RecordInputOutcome::Stop;
            }
        }
    } else if !blocked_input || accepted_user_input {
        for item in prefix {
            session
                .record_conversation_items(turn_context, std::slice::from_ref(&item))
                .await;
        }
    }
    for (index, (input_item, hook_outcome)) in inspected.into_iter().enumerate() {
        if hook_outcome.should_stop || atomic_input_indices.contains(&index) {
            record_additional_contexts(session, turn_context, hook_outcome.additional_contexts)
                .await;
            continue;
        }
        record_pending_input(
            session,
            turn_context,
            input_item,
            hook_outcome.additional_contexts,
        )
        .await;
    }
    if blocked_input && !accepted_user_input {
        RecordInputOutcome::Stop
    } else {
        RecordInputOutcome::Continue
    }
}

pub(super) async fn pending_for_input(
    session: &Session,
    input: &[TurnInput],
) -> (Vec<ResponseItem>, Option<String>, Option<String>) {
    if !input
        .iter()
        .any(|item| matches!(item, TurnInput::UserInput { content, .. } if !content.is_empty()))
    {
        return (Vec::new(), None, None);
    }
    let Some(handoff) = session.pending_review_handoff().await else {
        return (Vec::new(), None, None);
    };
    let through = handoff.through_item_id().to_string();
    let transaction = handoff.transaction_id().to_string();
    (
        handoff.into_response_items(),
        Some(through),
        Some(transaction),
    )
}

pub(super) async fn record_first_accepted_input(
    session: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    handoff_items: Vec<ResponseItem>,
    transaction_id: &str,
    through_item_id: &str,
    inspected: &[(TurnInput, HookRuntimeOutcome)],
) -> anyhow::Result<Vec<usize>> {
    let (index, content, client_id) = inspected
        .iter()
        .enumerate()
        .find_map(|(index, (input_item, outcome))| match input_item {
            TurnInput::UserInput { content, client_id }
                if !outcome.should_stop && !content.is_empty() =>
            {
                Some((index, content.as_slice(), client_id.clone()))
            }
            TurnInput::UserInput { .. }
            | TurnInput::ResponseItem(_)
            | TurnInput::InterAgentCommunication(_) => None,
        })
        .ok_or_else(|| anyhow::anyhow!("review handoff has no accepted user input"))?;
    let mut recorded_indices = Vec::new();
    let mut leading_input = Vec::new();
    for (leading_index, (input_item, outcome)) in inspected[..index].iter().enumerate() {
        if outcome.should_stop {
            continue;
        }
        match input_item {
            TurnInput::ResponseItem(item) => {
                leading_input.push(ReviewHandoffLeadingInput::ResponseItem(item.clone()));
                recorded_indices.push(leading_index);
            }
            TurnInput::InterAgentCommunication(communication) => {
                leading_input.push(ReviewHandoffLeadingInput::InterAgentCommunication(
                    communication.clone(),
                ));
                recorded_indices.push(leading_index);
            }
            TurnInput::UserInput { .. } => {}
        }
    }
    session
        .record_review_handoff_and_user_prompt(
            turn_context.as_ref(),
            leading_input,
            handoff_items,
            transaction_id,
            through_item_id,
            content,
            client_id,
        )
        .await?;
    recorded_indices.push(index);
    Ok(recorded_indices)
}
