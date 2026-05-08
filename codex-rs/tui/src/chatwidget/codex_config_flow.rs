use super::*;
use crate::codex_config_context::CodexRequestMode;

const APPLY_USER_MESSAGE: &str = "Apply approved Codex config changes.";

impl ChatWidget {
    pub(super) fn strip_codex_config_done_marker(&mut self, message: String) -> (String, bool) {
        if !matches!(
            self.active_mode_kind(),
            ModeKind::Codex | ModeKind::CodexConfigEdit
        )
            || !message.contains(crate::codex_config_context::CODEX_CONFIG_DONE_MARKER)
        {
            return (message, false);
        }

        self.saw_codex_config_done_this_turn = true;
        let message = message
            .replace(crate::codex_config_context::CODEX_CONFIG_DONE_MARKER, "")
            .trim_end()
            .to_string();
        (message, true)
    }

    pub(super) fn apply_codex_request_mode_for_text(&mut self, text: &str) {
        if self.pending_codex_config_apply_bundle.is_some() {
            return;
        }
        if !matches!(
            self.active_mode_kind(),
            ModeKind::Codex | ModeKind::CodexConfigEdit
        ) {
            return;
        }

        match crate::codex_config_context::classify_codex_request(text) {
            CodexRequestMode::Investigate => {
                if self.active_mode_kind() != ModeKind::Codex {
                    self.set_collaboration_mask(crate::codex_config_context::codex_investigate_mask(
                        &self.config.cwd,
                    ));
                }
            }
            CodexRequestMode::ConfigEdit => {
                if self.active_mode_kind() != ModeKind::CodexConfigEdit {
                    self.codex_config_planning_conversation.clear();
                    self.latest_proposed_plan_markdown = None;
                    self.set_collaboration_mask(
                        crate::codex_config_context::codex_config_edit_mask(&self.config.cwd),
                    );
                }
            }
            CodexRequestMode::AiResolve => {
                self.codex_config_planning_conversation.clear();
                self.latest_proposed_plan_markdown = None;
                self.set_collaboration_mask(crate::codex_config_context::codex_ai_resolve_mask(
                    &self.config.cwd,
                ));
            }
        }
    }

    pub(super) fn record_codex_config_user_message(&mut self, text: &str) {
        if self.active_mode_kind() != ModeKind::CodexConfigEdit {
            return;
        }
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        append_conversation_section(
            &mut self.codex_config_planning_conversation,
            "User",
            text,
        );
    }

    pub(super) fn record_codex_config_plan(&mut self, plan: &str) {
        if self.active_mode_kind() != ModeKind::CodexConfigEdit {
            return;
        }
        let plan = plan.trim();
        if plan.is_empty() {
            return;
        }
        append_conversation_section(
            &mut self.codex_config_planning_conversation,
            "Assistant Proposed Plan",
            plan,
        );
    }

    pub(super) fn codex_turn_execution_context(
        &self,
    ) -> (PathBuf, SandboxPolicy, Option<PermissionProfile>) {
        let workspace =
            crate::codex_config_context::codex_config_workspace_for_cwd(&self.config.cwd);
        if self.pending_codex_config_apply_bundle.is_some() {
            return (
                workspace,
                crate::codex_config_context::codex_config_apply_sandbox_policy(
                    &self.config.codex_home,
                ),
                Some(crate::codex_config_context::codex_config_apply_permission_profile(
                    &self.config.codex_home,
                )),
            );
        }

        (
            workspace,
            crate::codex_config_context::codex_config_sandbox_policy(),
            Some(crate::codex_config_context::codex_config_permission_profile()),
        )
    }

    pub(super) fn maybe_prompt_codex_config_apply(&mut self) {
        if self.active_mode_kind() != ModeKind::CodexConfigEdit {
            return;
        }
        if !self.saw_plan_item_this_turn {
            return;
        }
        if self.has_queued_follow_up_messages() {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }

        self.open_codex_config_apply_prompt();
    }

    pub(super) fn open_codex_config_apply_prompt(&mut self) {
        let investigate_mask =
            crate::codex_config_context::codex_investigate_mask(&self.config.cwd);
        let default_mask = collaboration_modes::default_mode_mask(self.model_catalog.as_ref());
        self.bottom_pane
            .show_selection_view(codex_config_apply::selection_view_params(
                investigate_mask,
                default_mask,
                self.latest_proposed_plan_markdown.as_deref(),
            ));
    }

    pub(super) fn maybe_prompt_codex_config_completion(&mut self) {
        if !matches!(
            self.active_mode_kind(),
            ModeKind::Codex | ModeKind::CodexConfigEdit
        ) {
            return;
        }
        if !self.saw_codex_config_done_this_turn {
            return;
        }
        if self.saw_plan_item_this_turn {
            return;
        }
        if self.has_queued_follow_up_messages() {
            return;
        }
        if !self.bottom_pane.no_modal_or_popup_active() {
            return;
        }

        self.open_codex_config_completion_prompt();
    }

    pub(super) fn open_codex_config_completion_prompt(&mut self) {
        let default_mask = collaboration_modes::default_mode_mask(self.model_catalog.as_ref());
        let stay_mask = (self.active_mode_kind() == ModeKind::CodexConfigEdit)
            .then(|| crate::codex_config_context::codex_investigate_mask(&self.config.cwd));
        self.bottom_pane.show_selection_view(
            codex_config_completion::selection_view_params(default_mask, stay_mask),
        );
    }

    pub(crate) fn apply_codex_config_plan(&mut self) {
        let Some(plan) = self
            .latest_proposed_plan_markdown
            .as_deref()
            .map(str::trim)
            .filter(|plan| !plan.is_empty())
        else {
            self.add_error_message("No approved Codex config plan is available.".to_string());
            return;
        };

        let conversation = if self.codex_config_planning_conversation.trim().is_empty() {
            format!("## Assistant Proposed Plan\n\n{plan}\n")
        } else {
            self.codex_config_planning_conversation.clone()
        };

        let bundle = match crate::codex_config_history::create_bundle(
            self.config.codex_home.as_path(),
            self.thread_id,
            plan,
            &conversation,
        ) {
            Ok(bundle) => bundle,
            Err(err) => {
                self.add_error_message(format!(
                    "Failed to create Codex config history bundle: {err}"
                ));
                return;
            }
        };

        let user_text = format!(
            "{APPLY_USER_MESSAGE}\n\nConfig history bundle: `{}`\nPrimary config path: `{}`\nApproved plan file: `{}`\nRollback template: `{}`\n\nUse the approved plan from the bundle. Keep rollback data current if you touch any additional Codex config files.",
            bundle.path.display(),
            bundle.config_path.display(),
            bundle.path.join("approved-plan.md").display(),
            bundle.path.join("rollback.md").display()
        );
        let mask = crate::codex_config_context::codex_apply_mask(
            &self.config.cwd,
            &bundle.path,
            &bundle.config_path,
        );
        self.pending_codex_config_apply_bundle = Some(bundle);
        self.submit_user_message_with_mode(user_text, mask);
    }

    pub(super) fn finalize_codex_config_apply_if_needed(&mut self, final_message: &str) {
        let Some(bundle) = self.pending_codex_config_apply_bundle.take() else {
            return;
        };
        let result = crate::codex_config_history::finalize_bundle(&bundle, final_message);
        self.set_collaboration_mask(crate::codex_config_context::codex_investigate_mask(
            &self.config.cwd,
        ));
        match result {
            Ok(()) => self.add_info_message(
                format!("Codex config history saved: {}", bundle.path.display()),
                /*hint*/ None,
            ),
            Err(err) => self.add_error_message(format!(
                "Failed to finalize Codex config history bundle: {err}"
            )),
        }
    }
}

fn append_conversation_section(conversation: &mut String, title: &str, body: &str) {
    if !conversation.is_empty() {
        conversation.push_str("\n\n");
    }
    conversation.push_str("## ");
    conversation.push_str(title);
    conversation.push_str("\n\n");
    conversation.push_str(body.trim());
    conversation.push('\n');
}
