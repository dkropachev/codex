use codex_protocol::config_types::CollaborationModeMask;

use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;

pub(super) const CODEX_CONFIG_APPLY_TITLE: &str = "Apply Codex config changes?";
pub(super) const CODEX_CONFIG_APPLY_NO_PLAN: &str = "No approved plan available";

pub(super) fn selection_view_params(
    investigate_mask: CollaborationModeMask,
    default_mask: Option<CollaborationModeMask>,
    plan_markdown: Option<&str>,
) -> SelectionViewParams {
    let (apply_actions, apply_disabled_reason) = match plan_markdown {
        Some(plan) if !plan.trim().is_empty() => {
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(AppEvent::ApplyCodexConfigPlan);
            })];
            (actions, None)
        }
        _ => (Vec::new(), Some(CODEX_CONFIG_APPLY_NO_PLAN.to_string())),
    };

    let investigate_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
        tx.send(AppEvent::UpdateCollaborationMode(investigate_mask.clone()));
    })];

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
        title: Some(CODEX_CONFIG_APPLY_TITLE.to_string()),
        subtitle: None,
        footer_hint: Some(standard_popup_hint_line()),
        items: vec![
            SelectionItem {
                name: "Apply Codex config changes".to_string(),
                description: Some("Create history bundle and apply the approved plan.".to_string()),
                actions: apply_actions,
                disabled_reason: apply_disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Return to Codex investigate".to_string(),
                description: Some("Keep Codex mode read-only.".to_string()),
                actions: investigate_actions,
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Exit Codex mode".to_string(),
                description: Some("Return to Default mode for normal work.".to_string()),
                actions: exit_actions,
                disabled_reason: exit_disabled_reason,
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}
