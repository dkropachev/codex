use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_string::take_bytes_at_char_boundary;

use super::ContextualUserFragment;
use crate::context_manager::MAX_MODEL_CONTEXT_ITEM_TOKENS;
use crate::context_manager::estimate_item_token_count;

const GUARDIAN_PROMPT_SERIALIZATION_HEADROOM_TOKENS: usize = 64;
const GUARDIAN_MAX_ENRICHED_ITEM_TOKENS: usize =
    MAX_MODEL_CONTEXT_ITEM_TOKENS - GUARDIAN_PROMPT_SERIALIZATION_HEADROOM_TOKENS;
const GUARDIAN_SHORT_CONTEXT_ITEM_MAX_BYTES: usize = 256;

/// Host-verified authorization sections supplied to a Guardian review request.
///
/// The shared Guardian context crate owns collection and section markers. Core owns the
/// model-visible fragment boundary and delivery role.
pub struct GuardianAuthorizationContext(String);

impl GuardianAuthorizationContext {
    pub fn from_section(section: String) -> Self {
        Self(section)
    }

    pub fn from_sections(sections: Vec<String>) -> Self {
        Self(sections.concat())
    }
}

impl ContextualUserFragment for GuardianAuthorizationContext {
    fn role(&self) -> &'static str {
        "user"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("guardian.authorization_context".to_string())
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        self.0.clone()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }
}

/// Bounds one complete Guardian user message while reserving its approval-request suffix.
///
/// Context evidence may be reduced and images may be dropped, but the exact approval-request
/// suffix is retained. Failure prevents either a partial action or an oversized item from reaching
/// a model.
pub fn bound_guardian_model_input(
    items: &mut Vec<UserInput>,
    approval_request_start: usize,
) -> Result<(), String> {
    if guardian_model_input_tokens(items) <= GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
        return Ok(());
    }

    let approval_request_start = approval_request_start.min(items.len());
    let context = items[..approval_request_start].to_vec();
    let approval_request = items[approval_request_start..].to_vec();
    if guardian_model_input_tokens(&approval_request) > GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
        return Err("Guardian approval request exceeds the model-context item limit".into());
    }

    for include_images in [true, false] {
        if let Some(bounded) =
            best_guardian_context_candidate(&context, &approval_request, include_images)
        {
            *items = bounded;
            return Ok(());
        }
    }

    // The approval request already fit on its own. If preserving short trust-boundary items still
    // leaves too much per-content metadata, drop all prior context instead of malformed fragments.
    *items = approval_request;
    Ok(())
}

fn best_guardian_context_candidate(
    context: &[UserInput],
    approval_request: &[UserInput],
    include_images: bool,
) -> Option<Vec<UserInput>> {
    let max_context_item_tokens = context
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(approx_token_count(text)),
            _ => None,
        })
        .max()
        .unwrap_or_default();
    let mut low = 0usize;
    let mut high = max_context_item_tokens.saturating_add(1);
    let mut best = None;
    while low < high {
        let per_item_tokens = low + (high - low) / 2;
        let candidate =
            guardian_context_candidate(context, approval_request, include_images, per_item_tokens);
        if guardian_model_input_tokens(&candidate) <= GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
            best = Some(candidate);
            low = per_item_tokens.saturating_add(1);
        } else {
            high = per_item_tokens;
        }
    }
    best
}

fn guardian_context_candidate(
    context: &[UserInput],
    approval_request: &[UserInput],
    include_images: bool,
    per_item_tokens: usize,
) -> Vec<UserInput> {
    let mut candidate = Vec::with_capacity(context.len().saturating_add(approval_request.len()));
    for item in context {
        match item {
            UserInput::Text { text, .. } => {
                if text.len() <= GUARDIAN_SHORT_CONTEXT_ITEM_MAX_BYTES
                    || approx_token_count(text) <= per_item_tokens
                {
                    candidate.push(item.clone());
                } else {
                    candidate.push(UserInput::Text {
                        text: truncate_guardian_context_text(text, per_item_tokens),
                        text_elements: Vec::new(),
                    });
                }
            }
            UserInput::Image { .. } if include_images => candidate.push(item.clone()),
            UserInput::Image { .. } => {}
            _ => debug_assert!(
                false,
                "Guardian prompts only contain text and prepared images"
            ),
        }
    }
    candidate.extend_from_slice(approval_request);
    candidate
}

fn truncate_guardian_context_text(text: &str, max_tokens: usize) -> String {
    const MARKER: &str = "<truncated />";

    let max_bytes = approx_bytes_for_tokens(max_tokens);
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let marker_bytes = MARKER.len().saturating_mul(2);
    if max_bytes <= marker_bytes {
        return take_bytes_at_char_boundary(text, max_bytes).to_string();
    }

    let retained_bytes = max_bytes - marker_bytes;
    let prefix_bytes = retained_bytes / 3;
    let middle_bytes = retained_bytes / 3;
    let suffix_bytes = retained_bytes - prefix_bytes - middle_bytes;
    let prefix = take_bytes_at_char_boundary(text, prefix_bytes);

    let mut middle_start = text.len().saturating_sub(middle_bytes).saturating_div(2);
    while !text.is_char_boundary(middle_start) {
        middle_start = middle_start.saturating_add(1);
    }
    let mut middle_end = middle_start.saturating_add(middle_bytes).min(text.len());
    while !text.is_char_boundary(middle_end) {
        middle_end = middle_end.saturating_sub(1);
    }

    let mut suffix_start = text.len().saturating_sub(suffix_bytes);
    while !text.is_char_boundary(suffix_start) {
        suffix_start = suffix_start.saturating_add(1);
    }
    format!(
        "{prefix}{MARKER}{}{MARKER}{}",
        &text[middle_start..middle_end],
        &text[suffix_start..]
    )
}

pub(crate) fn guardian_model_input_tokens(items: &[UserInput]) -> usize {
    let mut item: ResponseItem = ResponseInputItem::from(items.to_vec()).into();
    if let ResponseItem::Message {
        content,
        internal_chat_message_metadata_passthrough,
        ..
    } = &mut item
    {
        let content_item_kinds = content
            .iter()
            .map(|content| {
                ContentItemKind(
                    match content {
                        ContentItem::InputText { .. } | ContentItem::OutputText { .. } => {
                            "user.text"
                        }
                        ContentItem::InputImage { .. } => "user.image",
                        ContentItem::InputAudio { .. } => "user.audio",
                    }
                    .to_string(),
                )
            })
            .collect();
        *internal_chat_message_metadata_passthrough =
            Some(InternalChatMessageMetadataPassthrough {
                turn_id: Some("00000000-0000-7000-8000-000000000000".to_string()),
                // Deliberately longer than a contemporary fractional Unix timestamp.
                create_time: Some(serde_json::Number::from(999_999_999_999_999_999_i64)),
                content_item_kinds: Some(content_item_kinds),
                ..Default::default()
            });
    }
    item.set_id(Some(ResponseItemId::with_suffix(
        "msg",
        "00000000-0000-7000-8000-000000000000",
    )));
    usize::try_from(estimate_item_token_count(&item)).unwrap_or(usize::MAX)
}
