use super::Turn;
use super::shared::v2_enum_from_core;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

v2_enum_from_core!(
    pub enum ReviewDelivery from codex_protocol::protocol::ReviewDelivery {
        Inline, Detached
    }
);

v2_enum_from_core!(
    pub enum ReviewVerification from codex_protocol::protocol::ReviewVerification {
        SinglePass, DoubleCheck
    }
);

v2_enum_from_core!(
    pub enum ReviewAction from codex_protocol::protocol::ReviewAction {
        Report, Fix, FixAndCommit
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: ReviewTarget,

    /// Where to run the review: inline (default) on the current thread or
    /// detached on a new thread (returned in `reviewThreadId`).
    #[serde(default)]
    #[ts(optional = nullable)]
    pub delivery: Option<ReviewDelivery>,

    /// Whether to verify discovery candidates in a second isolated stage.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub verification: Option<ReviewVerification>,

    /// What to do after the report is ready.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub action: Option<ReviewAction>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewStartResponse {
    pub turn: Turn,
    /// Identifies the thread where the review runs.
    ///
    /// For inline reviews, this is the original thread id.
    /// For detached reviews, this is the id of the new review thread.
    pub review_thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewResolveScopeParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewResolveScopeResponse {
    /// Open pull request associated with the selected checkout, when one was found.
    pub pull_request: Option<ReviewScopePullRequest>,
    /// Detected repository default branch, when one was found.
    pub default_branch: Option<ReviewScopeBranch>,
    /// Currently checked-out branch, or `null` for a detached head or non-repository cwd.
    pub current_branch: Option<String>,
    /// Available explicit base-branch targets, with the preferred target first.
    pub branches: Vec<String>,
    /// Whether staged, unstaged, or untracked changes are present.
    pub has_uncommitted_changes: bool,
    /// Recent commits reachable from HEAD, capped at 100 entries.
    pub commits: Vec<ReviewScopeCommit>,
    /// Short diagnostic shown when Git repository detection failed.
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewScopePullRequest {
    #[ts(type = "number")]
    pub number: u64,
    pub url: String,
    /// Display name of the pull request's base branch, when supplied by GitHub.
    pub base_branch: Option<String>,
    /// Exact local or remote ref for the pull request base, when it resolved unambiguously.
    pub base_branch_target: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewScopeBranch {
    /// Human-readable branch name presented to the user.
    pub display_name: String,
    /// Exact local or remote ref used as the review target.
    pub target: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ReviewScopeCommit {
    pub sha: String,
    pub title: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", export_to = "v2/")]
pub enum ReviewTarget {
    /// Review the working tree: staged, unstaged, and untracked files.
    UncommittedChanges,

    /// Review changes between the current branch and the given base branch.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    BaseBranch { branch: String },

    /// Review the changes introduced by a specific commit.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Commit {
        sha: String,
        /// Optional human-readable label (e.g., commit subject) for UIs.
        title: Option<String>,
    },

    /// Review the changes associated with a pull request.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    PullRequest { url: String },

    /// Review the accessible checkout without a comparison baseline.
    WholeRepository,

    /// Arbitrary instructions, equivalent to the old free-form prompt.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom { instructions: String },
}
