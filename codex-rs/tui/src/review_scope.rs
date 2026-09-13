//! App-server-backed review-scope discovery for the TUI picker.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ReviewResolveScopeParams;
use codex_app_server_protocol::ReviewResolveScopeResponse;
use codex_app_server_protocol::ReviewScopeCommit;
use codex_protocol::ThreadId;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewPullRequest {
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) base_branch: Option<String>,
    pub(crate) base_branch_target: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReviewScopeResolution {
    pub(crate) pull_request: Option<ReviewPullRequest>,
    pub(crate) default_branch: Option<String>,
    pub(crate) default_branch_target: Option<String>,
    pub(crate) current_branch: Option<String>,
    pub(crate) branches: Vec<String>,
    pub(crate) has_uncommitted_changes: bool,
    pub(crate) commits: Vec<ReviewScopeCommit>,
    pub(crate) error: Option<String>,
}

impl From<ReviewResolveScopeResponse> for ReviewScopeResolution {
    fn from(response: ReviewResolveScopeResponse) -> Self {
        let default_branch = response.default_branch;
        Self {
            pull_request: response.pull_request.map(|pull_request| ReviewPullRequest {
                number: pull_request.number,
                url: pull_request.url,
                base_branch: pull_request.base_branch,
                base_branch_target: pull_request.base_branch_target,
            }),
            default_branch: default_branch
                .as_ref()
                .map(|branch| branch.display_name.clone()),
            default_branch_target: default_branch.map(|branch| branch.target),
            current_branch: response.current_branch,
            branches: response.branches,
            has_uncommitted_changes: response.has_uncommitted_changes,
            commits: response.commits.into_iter().take(100).collect(),
            error: response.error,
        }
    }
}

/// Resolves review-scope picker data for a specific live thread.
///
/// Implementations must keep the request thread-scoped and return only repository metadata; scope
/// resolution must not submit model input or otherwise mutate the thread.
pub(crate) trait ReviewScopeResolver: Send + Sync {
    /// Loads the preferred review scope and explicit branch choices for `thread_id`.
    fn resolve(
        &self,
        thread_id: ThreadId,
    ) -> Pin<Box<dyn Future<Output = Result<ReviewScopeResolution, String>> + Send + '_>>;
}

pub(crate) type SharedReviewScopeResolver = Arc<dyn ReviewScopeResolver>;

/// Review-scope resolver backed by app-server's thread-aware executor selection.
#[derive(Clone)]
pub(crate) struct AppServerReviewScopeResolver {
    request_handle: AppServerRequestHandle,
}

impl AppServerReviewScopeResolver {
    pub(crate) fn new(request_handle: AppServerRequestHandle) -> Self {
        Self { request_handle }
    }
}

impl ReviewScopeResolver for AppServerReviewScopeResolver {
    fn resolve(
        &self,
        thread_id: ThreadId,
    ) -> Pin<Box<dyn Future<Output = Result<ReviewScopeResolution, String>> + Send + '_>> {
        Box::pin(async move {
            let response: ReviewResolveScopeResponse = self
                .request_handle
                .request_typed(ClientRequest::ReviewResolveScope {
                    request_id: RequestId::String(format!(
                        "review-resolve-scope-{}",
                        Uuid::new_v4()
                    )),
                    params: ReviewResolveScopeParams {
                        thread_id: thread_id.to_string(),
                    },
                })
                .await
                .map_err(|err| err.to_string())?;
            Ok(response.into())
        })
    }
}

#[cfg(test)]
#[path = "review_scope_tests.rs"]
mod tests;
