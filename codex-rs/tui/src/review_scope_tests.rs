use codex_app_server_protocol::ReviewResolveScopeResponse;
use codex_app_server_protocol::ReviewScopeBranch;
use codex_app_server_protocol::ReviewScopeCommit;
use codex_app_server_protocol::ReviewScopePullRequest;
use pretty_assertions::assert_eq;

use super::*;

#[test]
fn maps_app_server_resolution_without_losing_exact_branch_target() {
    let response = ReviewResolveScopeResponse {
        pull_request: Some(ReviewScopePullRequest {
            number: 42,
            url: "https://github.com/openai/codex/pull/42".to_string(),
            base_branch: Some("main".to_string()),
            base_branch_target: Some("refs/remotes/upstream/main".to_string()),
        }),
        default_branch: Some(ReviewScopeBranch {
            display_name: "main".to_string(),
            target: "refs/remotes/upstream/main".to_string(),
        }),
        current_branch: Some("feature".to_string()),
        branches: vec![
            "refs/remotes/upstream/main".to_string(),
            "refs/heads/feature".to_string(),
        ],
        has_uncommitted_changes: true,
        commits: vec![ReviewScopeCommit {
            sha: "abc1234".to_string(),
            title: "Example commit".to_string(),
        }],
        error: None,
    };

    assert_eq!(
        ReviewScopeResolution::from(response),
        ReviewScopeResolution {
            pull_request: Some(ReviewPullRequest {
                number: 42,
                url: "https://github.com/openai/codex/pull/42".to_string(),
                base_branch: Some("main".to_string()),
                base_branch_target: Some("refs/remotes/upstream/main".to_string()),
            }),
            default_branch: Some("main".to_string()),
            default_branch_target: Some("refs/remotes/upstream/main".to_string()),
            current_branch: Some("feature".to_string()),
            branches: vec![
                "refs/remotes/upstream/main".to_string(),
                "refs/heads/feature".to_string(),
            ],
            has_uncommitted_changes: true,
            commits: vec![ReviewScopeCommit {
                sha: "abc1234".to_string(),
                title: "Example commit".to_string(),
            }],
            error: None,
        }
    );
}
