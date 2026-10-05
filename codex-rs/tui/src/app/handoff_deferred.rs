//! App-level actions that would discard a deferred handoff when changing threads.

use super::*;
use crate::bottom_pane::SelectionAction;

impl App {
    pub(super) fn confirm_deferred_discard_app_event(&mut self, event: &AppEvent) -> bool {
        if !self.chat_widget.has_pending_deferred_handoff() {
            return false;
        }
        let action: Option<(&str, SelectionAction)> = match event {
            AppEvent::NewSession { name } => {
                let name = name.clone();
                Some((
                    "start a new session",
                    Box::new(move |tx| {
                        tx.send(AppEvent::NewSession { name: name.clone() });
                    }),
                ))
            }
            AppEvent::ClearUi { name } => {
                let name = name.clone();
                Some((
                    "clear this session",
                    Box::new(move |tx| {
                        tx.send(AppEvent::ClearUi { name: name.clone() });
                    }),
                ))
            }
            AppEvent::ClearUiAndSubmitUserMessage { text } => {
                let text = text.clone();
                Some((
                    "clear this session",
                    Box::new(move |tx| {
                        tx.send(AppEvent::ClearUiAndSubmitUserMessage { text: text.clone() });
                    }),
                ))
            }
            AppEvent::ForkCurrentSession { name } => {
                let name = name.clone();
                Some((
                    "fork this session",
                    Box::new(move |tx| {
                        tx.send(AppEvent::ForkCurrentSession { name: name.clone() });
                    }),
                ))
            }
            AppEvent::RevertSessionForPromptEdit {
                thread_id,
                selected_cell,
                prompt,
            } => {
                let thread_id = *thread_id;
                let selected_cell = Arc::clone(selected_cell);
                let prompt = prompt.clone();
                Some((
                    "edit an earlier prompt",
                    Box::new(move |tx| {
                        tx.send(AppEvent::RevertSessionForPromptEdit {
                            thread_id,
                            selected_cell: Arc::clone(&selected_cell),
                            prompt: prompt.clone(),
                        });
                    }),
                ))
            }
            AppEvent::StartManagedWorktree { mode, name } => {
                let mode = *mode;
                let name = name.clone();
                Some((
                    "start a managed worktree",
                    Box::new(move |tx| {
                        tx.send(AppEvent::StartManagedWorktree {
                            mode,
                            name: name.clone(),
                        });
                    }),
                ))
            }
            AppEvent::ArchiveCurrentThread => Some((
                "archive this session",
                Box::new(|tx| tx.send(AppEvent::ArchiveCurrentThread)),
            )),
            AppEvent::DeleteCurrentThread => Some((
                "delete this session",
                Box::new(|tx| tx.send(AppEvent::DeleteCurrentThread)),
            )),
            AppEvent::Exit(mode) if *mode != ExitMode::Immediate => {
                let mode = *mode;
                Some((
                    "quit Codex",
                    Box::new(move |tx| tx.send(AppEvent::Exit(mode))),
                ))
            }
            AppEvent::Logout => Some(("log out", Box::new(|tx| tx.send(AppEvent::Logout)))),
            _ => None,
        };
        action.is_some_and(|(label, action)| {
            self.chat_widget
                .confirm_deferred_discard_app_action(label, action)
        })
    }
}
