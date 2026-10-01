use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TruncationPolicy;
use codex_protocol::user_input::UserInput;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_output_truncation::truncate_text;

use super::ContextualUserFragment;
use crate::context_manager::MAX_MODEL_CONTEXT_ITEM_TOKENS;
use crate::context_manager::estimate_item_token_count;

const GUARDIAN_PROMPT_SERIALIZATION_HEADROOM_TOKENS: usize = 64;
const GUARDIAN_MAX_ENRICHED_ITEM_TOKENS: usize =
    MAX_MODEL_CONTEXT_ITEM_TOKENS - GUARDIAN_PROMPT_SERIALIZATION_HEADROOM_TOKENS;

/// Host-verified authorization sections supplied to a Guardian review request.
///
/// The shared Guardian context crate owns collection and section markers. Core owns the
/// model-visible fragment boundary and delivery role.
pub struct GuardianAuthorizationContext(Vec<String>);

impl GuardianAuthorizationContext {
    pub fn from_sections(sections: Vec<String>) -> Self {
        Self(sections)
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
        self.0.concat()
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
    let mut context = String::new();
    let mut approval_request = String::new();
    let mut images = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            UserInput::Text { text, .. } => {
                if index < approval_request_start {
                    context.push_str(text);
                } else {
                    approval_request.push_str(text);
                }
            }
            UserInput::Image { .. } => images.push(item.clone()),
            _ => debug_assert!(
                false,
                "Guardian prompts only contain text and prepared images"
            ),
        }
    }

    let approval_only = vec![UserInput::Text {
        text: approval_request.clone(),
        text_elements: Vec::new(),
    }];
    if guardian_model_input_tokens(&approval_only) > GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
        return Err("Guardian approval request exceeds the model-context item limit".into());
    }

    let mut context_budget =
        GUARDIAN_MAX_ENRICHED_ITEM_TOKENS.saturating_sub(approx_token_count(&approval_request));
    let mut bounded_context = truncate_text(&context, TruncationPolicy::Tokens(context_budget));
    let mut text = format!("{bounded_context}{approval_request}");
    let mut bounded = vec![UserInput::Text {
        text: text.clone(),
        text_elements: Vec::new(),
    }];
    bounded.extend(images);
    if guardian_model_input_tokens(&bounded) > GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
        bounded.truncate(1);
    }

    for _ in 0..4 {
        let item_tokens = guardian_model_input_tokens(&bounded);
        if item_tokens <= GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
            break;
        }
        context_budget = approx_token_count(&bounded_context)
            .saturating_sub(item_tokens.saturating_sub(GUARDIAN_MAX_ENRICHED_ITEM_TOKENS))
            .saturating_sub(16);
        bounded_context = truncate_text(&context, TruncationPolicy::Tokens(context_budget));
        text = format!("{bounded_context}{approval_request}");
        bounded[0] = UserInput::Text {
            text: text.clone(),
            text_elements: Vec::new(),
        };
    }

    if guardian_model_input_tokens(&bounded) > GUARDIAN_MAX_ENRICHED_ITEM_TOKENS {
        return Err("Guardian prompt could not be bounded to the model-context item limit".into());
    }
    *items = bounded;
    Ok(())
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
