use pretty_assertions::assert_eq;

use super::*;

#[tokio::test]
async fn accepted_user_input_invalidates_earlier_plan_in_live_and_replay_flows() {
    for from_replay in [false, true] {
        let (mut chat, _events, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
        chat.on_plan_item_completed("- Old plan".to_string(), "planning-turn".to_string());
        assert_eq!(
            (
                chat.transcript
                    .latest_authoritative_plan_markdown
                    .as_deref(),
                chat.transcript.latest_authoritative_plan_turn_id.as_deref(),
            ),
            (Some("- Old plan"), Some("planning-turn")),
        );

        chat.on_committed_user_message(
            &[UserInput::Text {
                text: "Please revise".to_string(),
                text_elements: Vec::new(),
            }],
            Some("accepted-steer"),
            from_replay,
            "planning-turn",
        );
        assert_eq!(
            (
                chat.transcript
                    .latest_authoritative_plan_markdown
                    .as_deref(),
                chat.transcript.latest_authoritative_plan_turn_id.as_deref(),
                chat.transcript.saw_plan_item_this_turn,
            ),
            (None, None, false),
        );

        chat.on_plan_item_completed("- Revised plan".to_string(), "planning-turn".to_string());
        assert_eq!(
            (
                chat.transcript
                    .latest_authoritative_plan_markdown
                    .as_deref(),
                chat.transcript.latest_authoritative_plan_turn_id.as_deref(),
            ),
            (Some("- Revised plan"), Some("planning-turn")),
        );
    }
}

#[tokio::test]
async fn empty_completed_plan_replaces_earlier_authority() {
    let (mut chat, _events, _ops) = make_chatwidget_manual(Some("gpt-5")).await;
    chat.on_plan_item_completed("- Old plan".to_string(), "planning-turn".to_string());
    chat.on_plan_item_completed(String::new(), "planning-turn".to_string());
    assert_eq!(
        (
            chat.transcript
                .latest_authoritative_plan_markdown
                .as_deref(),
            chat.transcript.latest_authoritative_plan_turn_id.as_deref(),
        ),
        (Some(""), Some("planning-turn")),
    );
}
