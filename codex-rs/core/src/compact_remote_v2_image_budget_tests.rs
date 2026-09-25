use super::*;
use crate::context::ContextualUserFragment;
use crate::context::ImageResizeNotice;
use crate::context::ImageResizeNoticeSource;
use crate::context::ResizedImage;
use codex_protocol::models::image_close_tag_text;
use codex_protocol::models::local_image_open_tag_text_with_path;
use pretty_assertions::assert_eq;

fn message(content: Vec<ContentItem>) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content,
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn image() -> ContentItem {
    ContentItem::InputImage {
        image_url: "data:image/png;base64,abc".to_string(),
        detail: None,
    }
}

fn text(value: &str) -> ContentItem {
    ContentItem::InputText {
        text: value.to_string(),
    }
}

fn trim(items: Vec<ResponseItem>, max_tokens: usize) -> Vec<ResponseItem> {
    truncate_retained_messages(
        items.into_iter().map(ResponseItemEnvelope::new).collect(),
        max_tokens,
        RetainedImageBudget::Enabled,
    )
    .into_iter()
    .map(ResponseItemEnvelope::into_item)
    .collect()
}

fn resize_notice(image_count: usize) -> ResponseItem {
    ContextualUserFragment::into(ImageResizeNotice::new(
        ImageResizeNoticeSource::UserMessage,
        vec![ResizedImage {
            image_number: 1,
            image_count,
            source_width: 3200,
            source_height: 3200,
            prepared_width: 2048,
            prepared_height: 2048,
        }],
    ))
}

#[test]
fn image_only_boundary_is_atomic_and_does_not_backfill_older_messages() {
    let newest = message(vec![text("new")]);
    let items = vec![
        message(vec![text("old")]),
        message(vec![image()]),
        newest.clone(),
    ];
    let image_tokens = images::content_item_token_count(&image());
    for (max_tokens, expected) in [
        (
            image_tokens + 1,
            vec![message(vec![image()]), newest.clone()],
        ),
        (image_tokens, vec![newest.clone()]),
        (1, vec![newest]),
    ] {
        assert_eq!(trim(items.clone(), max_tokens), expected);
    }
}

#[test]
fn later_image_parts_preserve_labels_and_audio() {
    let parts = vec![
        text("earlier text"),
        text(&local_image_open_tag_text_with_path(
            /*label_number*/ 7,
            std::path::Path::new("image.png"),
        )),
        image(),
        text(&image_close_tag_text()),
        text("keep"),
        ContentItem::InputAudio {
            audio_url: "data:audio/wav;base64,abc".to_string(),
        },
    ];
    let source = message(parts.clone());
    let image_tokens = parts[1..4]
        .iter()
        .map(images::content_item_token_count)
        .sum::<usize>();
    for (max_tokens, start) in [(image_tokens, 4), (image_tokens + 1, 1)] {
        let mut expected = source.clone();
        let ResponseItem::Message { content, .. } = &mut expected else {
            unreachable!()
        };
        *content = parts[start..].to_vec();
        assert_eq!(trim(vec![source.clone()], max_tokens), vec![expected]);
    }
}

#[test]
fn retained_image_message_respects_context_item_limit() {
    let source = message(vec![image(); 6]);
    let expected = message(vec![image(); 5]);

    let retained = trim(vec![source], RETAINED_MESSAGE_TOKEN_BUDGET);

    assert_eq!(retained, vec![expected]);
    assert!(
        estimate_item_token_count(&retained[0])
            <= i64::try_from(MAX_MODEL_CONTEXT_ITEM_TOKENS).unwrap_or(i64::MAX)
    );
}

#[test]
fn image_notice_is_retained_only_when_all_source_images_survive() {
    let source = message(vec![image(), image(), text("keep")]);
    let notice = resize_notice(/*image_count*/ 2);
    let retained_source = message(vec![image(), text("keep")]);
    let boundary_budget = message_text_token_count(&notice).max(1)
        + images::content_item_token_count(&image())
        + images::content_item_token_count(&text("keep"));

    assert_eq!(
        trim(vec![source.clone(), notice.clone()], boundary_budget),
        vec![retained_source]
    );
    assert_eq!(
        trim(
            vec![source.clone(), notice.clone()],
            RETAINED_MESSAGE_TOKEN_BUDGET
        ),
        vec![source, notice]
    );
}

#[test]
fn image_treatment_preserves_text_only_boundary_behavior() {
    let source = ResponseItemEnvelope::new(message(vec![
        text("earlier text"),
        text(&"middle".repeat(20)),
        text("later text"),
        ContentItem::InputAudio {
            audio_url: "data:audio/wav;base64,abc".to_string(),
        },
    ]));
    for max_tokens in [0, 1, 4, 16, 100] {
        assert_eq!(
            truncate_retained_messages(
                vec![source.clone()],
                max_tokens,
                RetainedImageBudget::Enabled,
            ),
            truncate_retained_messages_for_remote_compaction(vec![source.clone()], max_tokens),
        );
    }
}

#[test]
fn image_treatment_preserves_client_developer_boundary_behavior() {
    let mut item = message(vec![image(), text(&"a".repeat(1000))]);
    let ResponseItem::Message { role, .. } = &mut item else {
        unreachable!()
    };
    *role = "developer".to_string();
    let source = ResponseItemEnvelope {
        item,
        metadata: Some(CodexHarnessMetadata {
            client_authored: true,
        }),
    };
    for max_tokens in [1871, 1879, 1880, 1950, 2200] {
        assert_eq!(
            truncate_retained_messages(
                vec![source.clone()],
                max_tokens,
                RetainedImageBudget::Enabled
            ),
            truncate_retained_messages_for_remote_compaction(vec![source.clone()], max_tokens),
        );
    }
}
