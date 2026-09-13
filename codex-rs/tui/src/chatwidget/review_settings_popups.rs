//! Review verification and action selection surfaces.

use super::*;

const REVIEW_ACTION_VIEW_ID: &str = "review-action";

impl ChatWidget {
    pub(crate) fn show_review_verification_picker(
        &mut self,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        let items = [
            (
                "Don't double-check",
                "Use the discovery report directly.",
                ReviewVerification::SinglePass,
            ),
            (
                "Double-check",
                "Verify each candidate in a fresh review stage.",
                ReviewVerification::DoubleCheck,
            ),
        ]
        .into_iter()
        .map(|(name, description, verification)| {
            let target = target.clone();
            let cwd = cwd.clone();
            SelectionItem {
                name: name.to_string(),
                description: Some(description.to_string()),
                actions: vec![Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::OpenReviewActionPicker {
                        thread_id,
                        cwd: cwd.clone(),
                        target: target.clone(),
                        verification,
                    });
                })],
                dismiss_on_select: true,
                ..Default::default()
            }
        })
        .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Choose review verification".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(0),
            ..Default::default()
        });
    }

    pub(crate) fn show_review_action_picker(
        &mut self,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
        verification: ReviewVerification,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        if self.review.resolved_scope(&cwd).is_none() {
            let request_id = self.review.begin_scope_resolution(cwd.clone());
            self.bottom_pane.show_selection_view(SelectionViewParams {
                view_id: Some(REVIEW_ACTION_VIEW_ID),
                title: Some("Choose a review action".to_string()),
                items: vec![SelectionItem {
                    name: "Checking Git commit availability...".to_string(),
                    is_disabled: true,
                    ..Default::default()
                }],
                ..Default::default()
            });
            let resolver = self.review_scope_resolver.clone();
            let tx = self.app_event_tx.clone();
            tokio::spawn(async move {
                let resolution = match (resolver, thread_id) {
                    (Some(resolver), Some(thread_id)) => resolver
                        .resolve(thread_id)
                        .await
                        .unwrap_or_else(|_| crate::review_scope::ReviewScopeResolution {
                            error: Some("Could not detect Git review scopes.".to_string()),
                            ..Default::default()
                        }),
                    _ => crate::review_scope::ReviewScopeResolution {
                        error: Some("Could not detect Git review scopes.".to_string()),
                        ..Default::default()
                    },
                };
                tx.send(AppEvent::ReviewActionScopeResolved {
                    request_id,
                    thread_id,
                    cwd,
                    target,
                    verification,
                    resolution,
                });
            });
            return;
        }
        let commit_available = self
            .review
            .resolved_scope(&cwd)
            .is_some_and(|resolution| resolution.error.is_none() && !resolution.commits.is_empty());
        let params = self.review_action_picker_params(
            thread_id,
            cwd,
            target,
            verification,
            commit_available,
        );
        self.bottom_pane.show_selection_view(params);
    }

    fn review_action_picker_params(
        &self,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
        verification: ReviewVerification,
        commit_available: bool,
    ) -> SelectionViewParams {
        let default_mode_available =
            crate::collaboration_modes::default_mode_mask(self.model_catalog.as_ref()).is_some();
        let mut choices = vec![(
            "Report findings",
            "Show the review in this conversation.",
            ReviewAction::Report,
        )];
        if default_mode_available {
            choices.push((
                "Fix findings",
                "Revalidate and fix every valid finding.",
                ReviewAction::Fix,
            ));
            if commit_available {
                choices.push((
                    "Fix findings + commit",
                    "Fix, verify, and create one focused commit.",
                    ReviewAction::FixAndCommit,
                ));
            }
        }
        let items = choices
            .into_iter()
            .map(|(name, description, action)| {
                let target = target.clone();
                let cwd = cwd.clone();
                SelectionItem {
                    name: name.to_string(),
                    description: Some(description.to_string()),
                    actions: vec![Box::new(move |tx: &AppEventSender| {
                        tx.send(AppEvent::StartReview {
                            thread_id,
                            cwd: cwd.clone(),
                            target: target.clone(),
                            verification,
                            action,
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        SelectionViewParams {
            view_id: Some(REVIEW_ACTION_VIEW_ID),
            title: Some("Choose a review action".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(0),
            ..Default::default()
        }
    }

    pub(crate) fn apply_review_action_scope_resolution(
        &mut self,
        request_id: uuid::Uuid,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
        verification: ReviewVerification,
        resolution: crate::review_scope::ReviewScopeResolution,
    ) {
        if self.thread_id != thread_id
            || self.config.cwd.as_path() != cwd
            || !self.review.scope_resolution_matches(request_id, &cwd)
        {
            return;
        }
        let commit_available = resolution.error.is_none() && !resolution.commits.is_empty();
        let params = self.review_action_picker_params(
            thread_id,
            cwd.clone(),
            target,
            verification,
            commit_available,
        );
        if self
            .bottom_pane
            .replace_selection_view_if_active(REVIEW_ACTION_VIEW_ID, params)
        {
            self.review
                .set_scope_resolution(request_id, cwd, resolution);
        }
    }
}
