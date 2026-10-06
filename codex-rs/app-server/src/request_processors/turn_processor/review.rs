use super::super::thread_processor::thread_from_stored_thread;
use super::*;
use codex_agent_extension::AgentInvocation;
use codex_agent_extension::AgentRun;
use codex_app_server_protocol::ReviewDelivery;
use codex_app_server_protocol::ReviewResolveScopeParams;
use codex_app_server_protocol::ReviewResolveScopeResponse;
use codex_app_server_protocol::ReviewScopeBranch;
use codex_app_server_protocol::ReviewScopeCommit;
use codex_app_server_protocol::ReviewScopePullRequest;
use codex_app_server_protocol::ReviewStartParams;
use codex_app_server_protocol::ReviewStartResponse;
use codex_app_server_protocol::ReviewTarget;
use codex_protocol::protocol::ReviewRequest;
use codex_protocol::protocol::ReviewTarget as CoreReviewTarget;
use codex_skills::system_cache_root_dir;

impl TurnRequestProcessor {
    pub(crate) async fn review_start(
        &self,
        request_id: &ConnectionRequestId,
        params: ReviewStartParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let ReviewStartParams {
            thread_id,
            target,
            delivery,
        } = params;
        match &target {
            ReviewTarget::BaseBranch { branch } if branch.trim().is_empty() => {
                return Err(invalid_request("branch must not be empty"));
            }
            ReviewTarget::Commit { sha, .. } if sha.trim().is_empty() => {
                return Err(invalid_request("sha must not be empty"));
            }
            ReviewTarget::PullRequest { url } if url.trim().is_empty() => {
                return Err(invalid_request("url must not be empty"));
            }
            ReviewTarget::Custom { instructions } if instructions.trim().is_empty() => {
                return Err(invalid_request("instructions must not be empty"));
            }
            ReviewTarget::UncommittedChanges
            | ReviewTarget::BaseBranch { .. }
            | ReviewTarget::Commit { .. }
            | ReviewTarget::PullRequest { .. }
            | ReviewTarget::Custom { .. } => {}
        }
        let (parent_thread_id, thread) = self.load_thread(&thread_id).await?;
        self.ensure_direct_input_allowed(request_id, thread.as_ref())
            .await?;
        let target = match target {
            ReviewTarget::UncommittedChanges => CoreReviewTarget::UncommittedChanges,
            ReviewTarget::BaseBranch { branch } => CoreReviewTarget::BaseBranch { branch },
            ReviewTarget::Commit { sha, title } => CoreReviewTarget::Commit { sha, title },
            ReviewTarget::PullRequest { url } => CoreReviewTarget::PullRequest { url },
            ReviewTarget::Custom { instructions } => CoreReviewTarget::Custom { instructions },
        };
        let display_text = codex_core::review_prompts::user_facing_hint(&target);
        if matches!(delivery, Some(ReviewDelivery::Detached)) {
            if matches!(
                thread.config_snapshot().await.history_mode,
                codex_protocol::protocol::ThreadHistoryMode::Paginated
            ) {
                return Err(invalid_request(
                    "paginated threads do not support detached review",
                ));
            }
            let target_prompt = match &target {
                CoreReviewTarget::Custom { instructions } => instructions.clone(),
                _ => format!("Review {display_text}."),
            };
            let review_skill_path = system_cache_root_dir(&self.config.codex_home)
                .join("review-agent")
                .join("SKILL.md");
            let prompt = format!(
                "Use [$review-agent]({}) for this review.\n\n{target_prompt}",
                review_skill_path.display()
            );
            self.start_detached_review(request_id, parent_thread_id, &prompt)
                .await?;
            return Ok(None);
        }
        let turn_id = self
            .submit_core_op(
                request_id,
                thread.as_ref(),
                Op::Review {
                    review_request: ReviewRequest {
                        target,
                        user_facing_hint: Some(display_text.clone()),
                    },
                },
            )
            .await
            .map_err(|err| internal_error(format!("failed to start review: {err}")))?;
        let turn = Turn {
            id: turn_id.clone(),
            items: vec![ThreadItem::UserMessage {
                id: turn_id,
                client_id: None,
                content: vec![V2UserInput::Text {
                    text: display_text,
                    text_elements: Vec::new(),
                }],
            }],
            items_view: TurnItemsView::NotLoaded,
            error: None,
            status: TurnStatus::InProgress,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        self.outgoing
            .send_response(
                request_id.clone(),
                ReviewStartResponse {
                    turn,
                    review_thread_id: thread_id,
                },
            )
            .await;
        Ok(None)
    }

    async fn start_detached_review(
        &self,
        request_id: &ConnectionRequestId,
        parent_thread_id: ThreadId,
        prompt: &str,
    ) -> Result<(), JSONRPCErrorError> {
        let mut config = self.config.as_ref().clone();
        if let Some(review_model) = &config.review_model {
            config.model = Some(review_model.clone());
        }
        let AgentRun {
            thread_id,
            thread: review_thread,
            turn_id,
        } = self
            .agent_runner
            .start(
                parent_thread_id,
                AgentInvocation {
                    config,
                    prompt: prompt.to_string(),
                    parent_trace: self.request_trace_context(request_id).await,
                },
            )
            .await
            .map_err(|err| internal_error(format!("failed to start detached review: {err}")))?;
        let fallback_provider = self.config.model_provider_id.as_str();
        let stored_thread = match review_thread
            .read_thread(
                /*include_archived*/ true, /*include_history*/ false,
            )
            .await
        {
            Ok(stored_thread) => {
                let (thread, _) =
                    thread_from_stored_thread(stored_thread, fallback_provider, &self.config.cwd);
                Some(thread)
            }
            Err(err) => {
                tracing::warn!("failed to load summary for review thread {thread_id}: {err}");
                None
            }
        };
        if let Some(mut thread) = stored_thread {
            thread.session_id = thread_id.to_string();
            self.thread_watch_manager
                .upsert_thread_silently(&thread.id)
                .await;
            thread.status = resolve_thread_status(
                self.thread_watch_manager
                    .loaded_status_for_thread(&thread.id)
                    .await,
                /*has_in_progress_turn*/ false,
            );
            self.outgoing
                .send_server_notification(ServerNotification::ThreadStarted(
                    thread_started_notification(thread),
                ))
                .await;
        }
        log_listener_attach_result(
            self.ensure_conversation_listener(
                thread_id,
                request_id.connection_id,
                /*raw_events_enabled*/ false,
            )
            .await,
            thread_id,
            request_id.connection_id,
            "review thread",
        );
        let turn = Turn {
            id: turn_id.clone(),
            items: vec![ThreadItem::UserMessage {
                id: turn_id,
                client_id: None,
                content: vec![V2UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
            }],
            items_view: TurnItemsView::NotLoaded,
            error: None,
            status: TurnStatus::InProgress,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        self.outgoing
            .send_response(
                request_id.clone(),
                ReviewStartResponse {
                    turn,
                    review_thread_id: thread_id.to_string(),
                },
            )
            .await;
        Ok(())
    }

    pub(crate) async fn review_resolve_scope(
        &self,
        params: ReviewResolveScopeParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (_, thread) = self.load_thread(&params.thread_id).await?;
        let resolution = thread
            .resolve_review_scope()
            .await
            .map_err(|err| internal_error(format!("failed to resolve review scope: {err}")))?;
        Ok(Some(
            ReviewResolveScopeResponse {
                pull_request: resolution.pull_request.map(|pr| ReviewScopePullRequest {
                    number: pr.number,
                    url: pr.url,
                    base_branch: pr.base_branch,
                    base_branch_target: pr.base_branch_target,
                }),
                default_branch: resolution.default_branch.map(|branch| ReviewScopeBranch {
                    display_name: branch.display_name,
                    target: branch.target,
                }),
                current_branch: resolution.current_branch,
                branches: resolution.branches,
                commits: resolution
                    .commits
                    .into_iter()
                    .map(|commit| ReviewScopeCommit {
                        sha: commit.sha,
                        subject: commit.subject,
                    })
                    .collect(),
            }
            .into(),
        ))
    }
}
