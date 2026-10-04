//! Older pages preserve newer events and wait for enough prompts during browsing.

use super::*;
use crate::app::test_support::make_test_app;
use codex_app_server_protocol::TurnItemsView;
use pretty_assertions::assert_eq;

fn turn(id: &str, status: TurnStatus, item_ids: &[&str]) -> Turn {
    Turn {
        id: id.to_string(),
        items: item_ids
            .iter()
            .map(|id| ThreadItem::UserMessage {
                id: id.to_string(),
                client_id: None,
                content: Vec::new(),
            })
            .collect(),
        items_view: TurnItemsView::Full,
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

#[test]
fn overlapping_history_keeps_live_turn_state_and_newer_items() {
    let mut current = vec![turn("shared", TurnStatus::InProgress, &["overlap", "live"])];
    let older = vec![
        turn("old", TurnStatus::Completed, &["first"]),
        turn("shared", TurnStatus::Completed, &["before", "overlap"]),
    ];
    merge_older_turns(&mut current, older);
    assert_eq!(
        current,
        vec![
            turn("old", TurnStatus::Completed, &["first"]),
            turn(
                "shared",
                TurnStatus::InProgress,
                &["before", "overlap", "live"]
            ),
        ],
    );
}

fn user_cell(message: &str) -> Arc<dyn HistoryCell> {
    Arc::new(crate::history_cell::new_user_prompt(
        message.to_string(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ))
}

#[tokio::test]
async fn browsing_waits_for_a_prompt_outside_the_initial_history_window() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    tui.set_owned_screen(/*owned*/ true)?;
    app.scrollback_has_older_history = true;
    app.transcript_cells = vec![
        user_cell(""),
        Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
            "recent answer".into(),
        ])),
    ];
    app.handle_backtrack_esc_key(&mut tui);
    app.handle_backtrack_esc_key(&mut tui);
    assert!(app.backtrack.overlay_preview_active && app.browsing_needs_history());
    app.prepend_older_transcript_cells(vec![
        user_cell(""),
        Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
            "earlier answer".into(),
        ])),
    ]);
    assert!(app.browsing_needs_history());
    app.prepend_older_transcript_cells(vec![
        user_cell(""),
        user_cell("older prompt"),
        user_cell("latest prompt"),
    ]);
    app.apply_backtrack_selection_internal(app.backtrack.nth_user_message);
    assert_eq!(
        (
            app.backtrack.nth_user_message,
            crate::app_backtrack::nth_user_position(
                &app.transcript_cells,
                app.backtrack.nth_user_message
            )
        ),
        (1, Some(2))
    );
    assert!(!app.browsing_needs_history());
    tui.set_owned_screen(/*owned*/ false)?;
    Ok(())
}
