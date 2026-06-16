#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRouterOutputOptimizationStatus {
    Candidate,
    Accepted,
    Declined,
    Optimized,
}

impl ToolRouterOutputOptimizationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolRouterOutputOptimizationStatus::Candidate => "candidate",
            ToolRouterOutputOptimizationStatus::Accepted => "accepted",
            ToolRouterOutputOptimizationStatus::Declined => "declined",
            ToolRouterOutputOptimizationStatus::Optimized => "optimized",
        }
    }

    pub(super) fn from_str(value: &str) -> Self {
        match value {
            "accepted" => ToolRouterOutputOptimizationStatus::Accepted,
            "declined" => ToolRouterOutputOptimizationStatus::Declined,
            "optimized" => ToolRouterOutputOptimizationStatus::Optimized,
            _ => ToolRouterOutputOptimizationStatus::Candidate,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterOutputOptimizationRecord {
    pub model_slug: String,
    pub model_provider: String,
    pub toolset_hash: String,
    pub router_schema_version: i64,
    pub tool_namespace: String,
    pub tool_name: String,
    pub suggestion_key: String,
    pub suggestion_label: String,
    pub status: ToolRouterOutputOptimizationStatus,
    pub observation_count: i64,
    pub recovery_count: i64,
    pub original_output_tokens: i64,
    pub returned_output_tokens: i64,
    pub candidate_output_tokens: i64,
    pub saved_output_tokens: i64,
    pub last_decision_reason: Option<String>,
}

#[derive(Debug)]
pub(super) struct OutputOptimizationSuggestion {
    pub(super) suggestion_key: String,
    pub(super) suggestion_label: String,
    pub(super) original_output_tokens: i64,
    pub(super) returned_output_tokens: i64,
    pub(super) candidate_output_tokens: i64,
    pub(super) saved_output_tokens: i64,
}

#[derive(Debug)]
pub(super) struct OutputOptimizationKey {
    pub(super) model_slug: String,
    pub(super) model_provider: String,
    pub(super) toolset_hash: String,
    pub(super) router_schema_version: i64,
    pub(super) tool_namespace: String,
    pub(super) tool_name: String,
}
