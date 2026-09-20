use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDelivery {
    Inline,
    Detached,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReviewVerification {
    #[default]
    SinglePass,
    DoubleCheck,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReviewAction {
    #[default]
    Report,
    Fix,
    FixAndCommit,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
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

    /// Arbitrary instructions provided by the user.
    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom { instructions: String },
}

/// Review request sent to the review session.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewRequest {
    pub target: ReviewTarget,
    #[serde(default)]
    pub verification: ReviewVerification,
    #[serde(default)]
    pub action: ReviewAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
}

/// Structured review result produced by the review agent chain.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS, Default)]
pub struct ReviewOutputEvent {
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub overall_correctness: String,
    #[serde(default)]
    pub overall_explanation: String,
    #[serde(default)]
    pub overall_confidence_score: f32,
    #[serde(default)]
    pub out_of_scope_findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub unverified_findings: Vec<ReviewFinding>,
    #[serde(default)]
    pub references: Vec<ReviewReference>,
    #[serde(default)]
    pub external_references: Vec<ReviewExternalReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resolution: Option<ReviewResolution>,
}

/// Whether a finding predates the selected review scope.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ReviewPreExisting {
    True,
    False,
    #[default]
    Undetermined,
}

/// A single review finding describing an observed issue or recommendation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewFinding {
    pub title: String,
    pub body: String,
    pub confidence_score: f32,
    pub priority: i32,
    pub code_location: ReviewCodeLocation,
    #[serde(default)]
    pub pre_existing: ReviewPreExisting,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pre_existing_fix_rationale: Option<String>,
}

/// Location of the code related to a review finding.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewCodeLocation {
    pub absolute_file_path: PathBuf,
    pub line_range: ReviewLineRange,
}

/// Inclusive line range in a file associated with a finding or context request.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ReviewLineRange {
    pub start: u32,
    pub end: u32,
}

/// An in-checkout source range that could not be loaded for verification.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ReviewReference {
    pub reference: String,
    pub explanation: String,
}

/// An external resource named by a reviewer but never fetched by the chain.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ReviewExternalReference {
    pub reference: String,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReviewResolutionStatus {
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ReviewTestStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ReviewTestResult {
    pub command: String,
    pub status: ReviewTestStatus,
}

/// Structured outcome from the optional fix stage.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ReviewResolution {
    pub status: ReviewResolutionStatus,
    pub fixed_count: usize,
    pub rejected_count: usize,
    pub unresolved_count: usize,
    pub summary: Vec<String>,
    pub tests: Vec<ReviewTestResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub commit_sha: Option<String>,
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
