use codex_protocol::config_types::CollaborationModeMask;

use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;

pub(super) const CODEX_CONFIG_COMPLETION_TITLE: &str = "Leave Codex config mode?";

pub(super) fn selection_view_params(
    default_mask: Option<CollaborationModeMask>,
) -> SelectionViewParams {
    let (exit_actions, exit_disabled_reason) = match default_mask {
        Some(mask) => {
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::UpdateCollaborationMode(mask.clone()));
            })];
            (actions, None)
        }
        None => (Vec::new(), Some("Default mode unavailable".to_string())),
    };

    SelectionViewParams {
        title: Some(CODEX_CONFIG_COMPLETION_TITLE.to_string()),
        subtitle: None,
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![
            SelectionItem {
                name: "Exit Codex config mode".to_string(),
                description: Some("Return to Default mode for normal work.".to_string()),
                actions: exit_actions,
                disabled_reason: exit_disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Stay in Codex config mode".to_string(),
                description: Some("Continue configuring Codex.".to_string()),
                actions: Vec::new(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}
