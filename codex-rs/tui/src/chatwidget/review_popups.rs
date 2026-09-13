//! Review scope, action, commit, and custom-instruction selection surfaces.

use super::*;

const REVIEW_SCOPE_VIEW_ID: &str = "review-scope";

impl ChatWidget {
    pub(crate) fn open_review_popup(&mut self) {
        let cwd = self.config.cwd.to_path_buf();
        let request_id = self.review.begin_scope_resolution(cwd.clone());
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(REVIEW_SCOPE_VIEW_ID),
            title: Some("Select a review scope".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items: vec![SelectionItem {
                name: "Loading review scopes...".to_string(),
                is_disabled: true,
                ..Default::default()
            }],
            ..Default::default()
        });

        let resolver = self.review_scope_resolver.clone();
        let thread_id = self.thread_id;
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let resolution = match (resolver, thread_id) {
                (Some(resolver), Some(thread_id)) => {
                    resolver.resolve(thread_id).await.unwrap_or_default()
                }
                _ => Default::default(),
            };
            tx.send(AppEvent::ReviewScopesResolved {
                request_id,
                cwd,
                resolution,
            });
        });
    }

    pub(crate) fn apply_review_scope_resolution(
        &mut self,
        request_id: uuid::Uuid,
        cwd: PathBuf,
        resolution: crate::review_scope::ReviewScopeResolution,
    ) -> bool {
        if self.config.cwd.as_path() != cwd
            || !self.review.scope_resolution_matches(request_id, &cwd)
        {
            return false;
        }

        let thread_id = self.thread_id;
        let mut items = Vec::new();
        let mut includes_uncommitted = false;
        if let Some(pull_request) = resolution.pull_request.as_ref() {
            let description = pull_request
                .base_branch
                .as_deref()
                .map(|branch| format!("Open PR into {branch}"))
                .or_else(|| Some("Open pull request".to_string()));
            items.push(review_target_item(
                format!("Review pull request #{}", pull_request.number),
                description,
                thread_id,
                cwd.clone(),
                ReviewTarget::PullRequest {
                    url: pull_request.url.clone(),
                },
            ));
        } else if let Some(default_branch) = resolution.default_branch.as_ref() {
            let branch_target = resolution
                .default_branch_target
                .clone()
                .unwrap_or_else(|| default_branch.clone());
            items.push(review_target_item(
                format!("Review changes against {default_branch}"),
                Some("Detected default branch".to_string()),
                thread_id,
                cwd.clone(),
                ReviewTarget::BaseBranch {
                    branch: branch_target,
                },
            ));
        } else {
            includes_uncommitted = true;
            items.push(review_target_item(
                "Review uncommitted changes".to_string(),
                /*description*/ None,
                thread_id,
                cwd.clone(),
                ReviewTarget::UncommittedChanges,
            ));
        }

        if !includes_uncommitted {
            items.push(review_target_item(
                "Review uncommitted changes".to_string(),
                /*description*/ None,
                thread_id,
                cwd.clone(),
                ReviewTarget::UncommittedChanges,
            ));
        }
        items.push(SelectionItem {
            name: "Choose a base branch".to_string(),
            description: Some("Select a specific branch".to_string()),
            actions: vec![Box::new({
                let cwd = cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewBranchPicker {
                        thread_id,
                        cwd: cwd.clone(),
                    })
                }
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Review a commit".to_string(),
            actions: vec![Box::new({
                let cwd = cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewCommitPicker {
                        thread_id,
                        cwd: cwd.clone(),
                    })
                }
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Custom review instructions".to_string(),
            actions: vec![Box::new({
                let cwd = cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewCustomPrompt {
                        thread_id,
                        cwd: cwd.clone(),
                    })
                }
            })],
            dismiss_on_select: false,
            dismiss_parent_on_child_accept: true,
            ..Default::default()
        });

        let replaced = self.bottom_pane.replace_selection_view_if_active(
            REVIEW_SCOPE_VIEW_ID,
            SelectionViewParams {
                view_id: Some(REVIEW_SCOPE_VIEW_ID),
                title: Some("Select a review scope".to_string()),
                footer_hint: Some(standard_popup_hint_line()),
                items,
                initial_selected_idx: Some(0),
                ..Default::default()
            },
        );
        if replaced {
            self.review
                .set_scope_resolution(request_id, cwd, resolution);
        }
        replaced
    }

    pub(crate) fn show_review_branch_picker(&mut self, thread_id: Option<ThreadId>, cwd: &Path) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        let resolution = self.review.resolved_scope(cwd).cloned().unwrap_or_default();
        let current_branch = resolution
            .current_branch
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        let pull_request_base = resolution
            .pull_request
            .as_ref()
            .and_then(|pull_request| pull_request.base_branch_target.as_deref())
            .map(str::to_string);
        let default_branch = resolution.default_branch;
        let default_branch_target = resolution.default_branch_target;
        let mut items = Vec::with_capacity(resolution.branches.len().max(1));
        let cwd = cwd.to_path_buf();

        for option in resolution.branches {
            let cwd = cwd.clone();
            let option_display = review_branch_display_name(&option);
            let is_default = default_branch.as_deref() == Some(option_display)
                || default_branch_target.as_ref() == Some(&option);
            let branch = if is_default {
                default_branch_target
                    .clone()
                    .unwrap_or_else(|| option.clone())
            } else {
                option.clone()
            };
            let display_branch = if is_default {
                default_branch.as_deref().unwrap_or(&option)
            } else {
                option_display
            };
            items.push(SelectionItem {
                name: format!("{current_branch} -> {display_branch}"),
                description: (pull_request_base.as_ref() == Some(&option))
                    .then(|| "Pull request base".to_string()),
                is_default,
                actions: vec![Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::OpenReviewActionPicker {
                        thread_id,
                        cwd: cwd.clone(),
                        target: ReviewTarget::BaseBranch {
                            branch: branch.clone(),
                        },
                    });
                })],
                dismiss_on_select: true,
                search_value: Some(option),
                ..Default::default()
            });
        }
        if items.is_empty() {
            items.push(SelectionItem {
                name: "No local branches found".to_string(),
                is_disabled: true,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            initial_selected_idx: Some(0),
            ..Default::default()
        });
    }

    pub(crate) async fn show_review_commit_picker(
        &mut self,
        thread_id: Option<ThreadId>,
        cwd: &Path,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        let commits = recent_commits(cwd, /*limit*/ 100).await;
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }

        let mut items: Vec<SelectionItem> = Vec::with_capacity(commits.len());
        let cwd = cwd.to_path_buf();
        for entry in commits {
            let cwd = cwd.clone();
            let subject = entry.subject.clone();
            let sha = entry.sha.clone();
            let search_val = format!("{subject} {sha}");

            items.push(SelectionItem {
                name: subject.clone(),
                actions: vec![Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::OpenReviewActionPicker {
                        thread_id,
                        cwd: cwd.clone(),
                        target: ReviewTarget::Commit {
                            sha: sha.clone(),
                            title: Some(subject.clone()),
                        },
                    });
                })],
                dismiss_on_select: true,
                search_value: Some(search_val),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a commit to review".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search commits".to_string()),
            ..Default::default()
        });
    }

    pub(crate) fn show_review_custom_prompt(&mut self, thread_id: Option<ThreadId>, cwd: &Path) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        let tx = self.app_event_tx.clone();
        let cwd = cwd.to_path_buf();
        let view = CustomPromptView::new(
            "Custom review instructions".to_string(),
            "Type instructions and press Enter".to_string(),
            /*initial_text*/ String::new(),
            /*context_label*/ None,
            Box::new(move |prompt: String| {
                let trimmed = prompt.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                tx.send(AppEvent::OpenReviewActionPicker {
                    thread_id,
                    cwd: cwd.clone(),
                    target: ReviewTarget::Custom {
                        instructions: trimmed,
                    },
                });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn show_review_action_picker(
        &mut self,
        thread_id: Option<ThreadId>,
        cwd: PathBuf,
        target: ReviewTarget,
    ) {
        if self.thread_id != thread_id || self.config.cwd.as_path() != cwd {
            return;
        }
        let default_mode_available =
            crate::collaboration_modes::default_mode_mask(self.model_catalog.as_ref()).is_some();
        let items = [
            (
                "Report findings",
                "Show the review in this conversation.",
                ReviewAction::Report,
            ),
            (
                "Fix findings",
                "Revalidate and fix every valid finding.",
                ReviewAction::Fix,
            ),
            (
                "Fix findings + commit",
                "Fix, verify, and create one focused commit.",
                ReviewAction::FixAndCommit,
            ),
        ]
        .into_iter()
        .map(|(name, description, action)| {
            let target = target.clone();
            let cwd = cwd.clone();
            let enabled = action == ReviewAction::Report || default_mode_available;
            SelectionItem {
                name: name.to_string(),
                description: Some(description.to_string()),
                actions: if enabled {
                    vec![Box::new(move |tx: &AppEventSender| {
                        tx.send(AppEvent::StartReview {
                            thread_id,
                            cwd: cwd.clone(),
                            target: target.clone(),
                            action,
                        });
                    }) as SelectionAction]
                } else {
                    Vec::new()
                },
                dismiss_on_select: true,
                disabled_reason: (!enabled).then(|| {
                    plan_implementation::PLAN_IMPLEMENTATION_DEFAULT_UNAVAILABLE.to_string()
                }),
                ..Default::default()
            }
        })
        .collect();
        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Choose a review action".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx: Some(0),
            ..Default::default()
        });
    }
}

fn review_target_item(
    name: String,
    description: Option<String>,
    thread_id: Option<ThreadId>,
    cwd: PathBuf,
    target: ReviewTarget,
) -> SelectionItem {
    SelectionItem {
        name,
        description,
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenReviewActionPicker {
                thread_id,
                cwd: cwd.clone(),
                target: target.clone(),
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn review_branch_display_name(branch: &str) -> &str {
    branch
        .strip_prefix("refs/heads/")
        .or_else(|| branch.strip_prefix("refs/remotes/"))
        .unwrap_or(branch)
}

#[cfg(test)]
pub(crate) fn show_review_commit_picker_with_entries(
    chat: &mut ChatWidget,
    entries: Vec<CommitLogEntry>,
) {
    let mut items: Vec<SelectionItem> = Vec::with_capacity(entries.len());
    let thread_id = chat.thread_id;
    let cwd = chat.config.cwd.to_path_buf();
    for entry in entries {
        let cwd = cwd.clone();
        let subject = entry.subject.clone();
        let sha = entry.sha.clone();
        let search_val = format!("{subject} {sha}");

        items.push(SelectionItem {
            name: subject.clone(),
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::OpenReviewActionPicker {
                    thread_id,
                    cwd: cwd.clone(),
                    target: ReviewTarget::Commit {
                        sha: sha.clone(),
                        title: Some(subject.clone()),
                    },
                });
            })],
            dismiss_on_select: true,
            search_value: Some(search_val),
            ..Default::default()
        });
    }

    chat.bottom_pane.show_selection_view(SelectionViewParams {
        title: Some("Select a commit to review".to_string()),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        is_searchable: true,
        search_placeholder: Some("Type to search commits".to_string()),
        ..Default::default()
    });
}
