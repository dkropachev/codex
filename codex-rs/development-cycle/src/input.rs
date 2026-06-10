use std::collections::BTreeMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::review_types::ReviewTypeDefinitionInput;
use crate::review_types::review_type_definitions_schema;

pub(crate) const DEFAULT_EXPERIMENT_SAMPLE_RATE: f64 = 0.20;
pub(crate) const DEFAULT_MIN_EVIDENCE_RUNS: u32 = 5;
pub(crate) const DEFAULT_MAX_REJECTED_GROUPING_EXPERIMENTS: u32 = 3;
pub(crate) const DEFAULT_MAX_REVIEW_ITEMS_PER_GROUP: u32 = 3;
pub(crate) const DEFAULT_BASELINE_RESAMPLE_RATE: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub(crate) enum TestMode {
    #[default]
    Auto,
    Provided,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevCycleInput {
    #[serde(default)]
    pub task_description: Option<String>,
    #[serde(default)]
    pub review_types: Option<Vec<String>>,
    #[serde(default)]
    pub review_type_definitions: Option<BTreeMap<String, ReviewTypeDefinitionInput>>,
    #[serde(default = "default_experiment_sample_rate")]
    pub experiment_sample_rate: f64,
    #[serde(default = "default_min_evidence_runs")]
    pub min_evidence_runs: u32,
    #[serde(default = "default_true")]
    pub effort_lowering_enabled: bool,
    #[serde(default = "default_true")]
    pub grouping_optimization_enabled: bool,
    #[serde(default = "default_max_rejected_grouping_experiments")]
    pub max_rejected_grouping_experiments: u32,
    #[serde(default = "default_max_review_items_per_group")]
    pub max_review_items_per_group: u32,
    #[serde(default = "default_baseline_resample_rate")]
    pub baseline_resample_rate: f64,
    #[serde(default = "default_review_parallelism")]
    pub max_parallel_review_agents: usize,
    #[serde(default = "default_writer_parallelism")]
    pub max_parallel_writers: usize,
    #[serde(default = "default_verifier_parallelism")]
    pub max_parallel_verifiers: usize,
    #[serde(default)]
    pub test_mode: TestMode,
    #[serde(default)]
    pub test_commands: Vec<String>,
    #[serde(default = "default_stage_mode")]
    pub default_stage_mode: String,
    #[serde(default = "default_integration_mode")]
    pub integration_mode: String,
    #[serde(default)]
    pub stages: BTreeMap<String, String>,
    #[serde(default = "default_commit_style")]
    pub commit_style: String,
    #[serde(default = "default_architecture_style")]
    pub architecture_style: String,
    #[serde(default = "default_coding_style")]
    pub coding_style: String,
    #[serde(default = "default_review_priorities")]
    pub review_priorities: String,
    #[serde(default = "default_ux_expectations")]
    pub ux_expectations: String,
    #[serde(default = "default_ui_expectations")]
    pub ui_expectations: String,
    #[serde(default = "default_test_expectations")]
    pub test_expectations: String,
}

pub(crate) fn parse_input(input: JsonValue) -> anyhow::Result<(DevCycleInput, JsonValue)> {
    let normalized = normalize_stage_aliases(input);
    let mut parsed = serde_json::from_value::<DevCycleInput>(normalized.clone())?;
    if !(0.0..=1.0).contains(&parsed.experiment_sample_rate) {
        anyhow::bail!("experimentSampleRate must be between 0.0 and 1.0");
    }
    if !(0.0..=1.0).contains(&parsed.baseline_resample_rate) {
        anyhow::bail!("baselineResampleRate must be between 0.0 and 1.0");
    }
    parsed.max_review_items_per_group = parsed.max_review_items_per_group.max(1);
    parsed.max_parallel_review_agents = parsed.max_parallel_review_agents.max(1);
    parsed.max_parallel_writers = parsed.max_parallel_writers.max(1);
    parsed.max_parallel_verifiers = parsed.max_parallel_verifiers.max(1);
    Ok((parsed, normalized))
}

pub(crate) fn default_input() -> JsonValue {
    json!({
        "taskDescription": null,
        "reviewTypes": null,
        "reviewTypeDefinitions": {},
        "experimentSampleRate": DEFAULT_EXPERIMENT_SAMPLE_RATE,
        "minEvidenceRuns": DEFAULT_MIN_EVIDENCE_RUNS,
        "effortLoweringEnabled": true,
        "groupingOptimizationEnabled": true,
        "maxRejectedGroupingExperiments": DEFAULT_MAX_REJECTED_GROUPING_EXPERIMENTS,
        "maxReviewItemsPerGroup": DEFAULT_MAX_REVIEW_ITEMS_PER_GROUP,
        "baselineResampleRate": DEFAULT_BASELINE_RESAMPLE_RATE,
        "maxParallelReviewAgents": 4,
        "maxParallelWriters": 2,
        "maxParallelVerifiers": 4,
        "testMode": "auto",
        "testCommands": [],
        "defaultStageMode": "auto",
        "integrationMode": "cherryPick",
        "stages": {
            "planning": "on",
            "architectureReview": "auto",
            "implementation": "on",
            "codeReview": "on",
            "uxReview": "auto",
            "uiReview": "auto",
            "tests": "on",
            "finalReview": "on",
            "integration": "on"
        },
        "commitStyle": default_commit_style(),
        "architectureStyle": default_architecture_style(),
        "codingStyle": default_coding_style(),
        "reviewPriorities": default_review_priorities(),
        "uxExpectations": default_ux_expectations(),
        "uiExpectations": default_ui_expectations(),
        "testExpectations": default_test_expectations()
    })
}

pub(crate) fn input_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "taskDescription": {
                "type": ["string", "null"],
                "description": "Task to implement. If omitted, the planner infers the task from the active thread context."
            },
            "reviewTypes": {
                "type": ["array", "null"],
                "items": { "type": "string" },
                "description": "Review type ids to run. If omitted, dev-cycle auto-selects from enabled review types."
            },
            "reviewTypeDefinitions": review_type_definitions_schema(),
            "experimentSampleRate": {
                "type": "number",
                "minimum": 0,
                "maximum": 1,
                "description": "Fraction of runs that sample challenger model/split strategies."
            },
            "minEvidenceRuns": {
                "type": "integer",
                "minimum": 1,
                "description": "Distinct completed runs required before experiment evidence can drive promotion or suppression."
            },
            "effortLoweringEnabled": {
                "type": "boolean",
                "description": "Whether lower reasoning effort experiments may run after high effort is proven good."
            },
            "groupingOptimizationEnabled": {
                "type": "boolean",
                "description": "Whether dev-cycle may ask the active review model to propose reviewer grouping experiments."
            },
            "maxRejectedGroupingExperiments": {
                "type": "integer",
                "minimum": 0,
                "description": "Rejected grouping experiments allowed for the same model, item set, and repo size before new proposals stop."
            },
            "maxReviewItemsPerGroup": {
                "type": "integer",
                "minimum": 1,
                "description": "Maximum review item count allowed in one AI-proposed reviewer group."
            },
            "baselineResampleRate": {
                "type": "number",
                "minimum": 0,
                "maximum": 1,
                "description": "Probability of running separate:v1 as a shadow baseline when grouped review is active."
            },
            "maxParallelReviewAgents": {
                "type": "integer",
                "minimum": 1
            },
            "maxParallelWriters": {
                "type": "integer",
                "minimum": 1
            },
            "maxParallelVerifiers": {
                "type": "integer",
                "minimum": 1
            },
            "testMode": {
                "type": "string",
                "enum": ["auto", "provided", "off"]
            },
            "testCommands": {
                "type": "array",
                "items": { "type": "string" }
            },
            "defaultStageMode": {
                "type": "string",
                "enum": ["auto", "on", "off"]
            },
            "integrationMode": {
                "type": "string",
                "enum": ["cherryPick", "manual"]
            },
            "stages": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "planning": stage_mode_schema(),
                    "architectureReview": stage_mode_schema(),
                    "implementation": stage_mode_schema(),
                    "codeReview": stage_mode_schema(),
                    "uxReview": stage_mode_schema(),
                    "uiReview": stage_mode_schema(),
                    "tests": stage_mode_schema(),
                    "finalReview": stage_mode_schema(),
                    "integration": stage_mode_schema()
                }
            },
            "stagePlanning": stage_mode_schema(),
            "stageArchitectureReview": stage_mode_schema(),
            "stageImplementation": stage_mode_schema(),
            "stageCodeReview": stage_mode_schema(),
            "stageUxReview": stage_mode_schema(),
            "stageUiReview": stage_mode_schema(),
            "stageTests": stage_mode_schema(),
            "stageFinalReview": stage_mode_schema(),
            "stageIntegration": stage_mode_schema(),
            "commitStyle": string_schema("Commit style instructions."),
            "architectureStyle": string_schema("Architecture style instructions."),
            "codingStyle": string_schema("Implementation style instructions."),
            "reviewPriorities": string_schema("Code review priority instructions."),
            "uxExpectations": string_schema("UX review expectations."),
            "uiExpectations": string_schema("UI review expectations."),
            "testExpectations": string_schema("Testing expectations.")
        }
    })
}

pub(crate) fn output_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["workflowId", "engine", "status", "selectedReviewTypes", "excludedReviewTypes", "stateDatabase"],
        "properties": {
            "workflowId": { "type": "string" },
            "engine": { "type": "string" },
            "status": { "type": "string" },
            "workingDirectory": { "type": "string" },
            "stateDatabase": { "type": "string" },
            "selectedReviewTypes": { "type": "array" },
            "excludedReviewTypes": { "type": "array" },
            "writerCommits": { "type": "array" },
            "verifiedFindings": { "type": "array" },
            "disregardedFindings": { "type": "array" },
            "testResults": { "type": "array" },
            "integrationBranch": { "type": ["string", "null"] },
            "experimentDecisions": { "type": "array" },
            "reviewSplit": {
                "type": "object",
                "additionalProperties": true
            }
        },
        "additionalProperties": true
    })
}

fn normalize_stage_aliases(mut input: JsonValue) -> JsonValue {
    let JsonValue::Object(input_object) = &mut input else {
        return input;
    };
    let mut stage_overrides = JsonMap::new();
    for (alias, stage) in STAGE_ALIASES {
        if let Some(value) = input_object.remove(*alias) {
            stage_overrides.insert((*stage).to_string(), value);
        }
    }
    if stage_overrides.is_empty() {
        return input;
    }

    let stages = input_object
        .entry("stages".to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    if let JsonValue::Object(stages) = stages {
        for (stage, value) in stage_overrides {
            stages.insert(stage, value);
        }
    }
    input
}

fn default_experiment_sample_rate() -> f64 {
    DEFAULT_EXPERIMENT_SAMPLE_RATE
}

fn default_min_evidence_runs() -> u32 {
    DEFAULT_MIN_EVIDENCE_RUNS
}

fn default_max_rejected_grouping_experiments() -> u32 {
    DEFAULT_MAX_REJECTED_GROUPING_EXPERIMENTS
}

fn default_max_review_items_per_group() -> u32 {
    DEFAULT_MAX_REVIEW_ITEMS_PER_GROUP
}

fn default_baseline_resample_rate() -> f64 {
    DEFAULT_BASELINE_RESAMPLE_RATE
}

fn default_true() -> bool {
    true
}

fn default_review_parallelism() -> usize {
    4
}

fn default_writer_parallelism() -> usize {
    2
}

fn default_verifier_parallelism() -> usize {
    4
}

fn default_stage_mode() -> String {
    "auto".to_string()
}

fn default_integration_mode() -> String {
    "cherryPick".to_string()
}

fn default_commit_style() -> String {
    "small, reviewable commits with clear messages".to_string()
}

fn default_architecture_style() -> String {
    "prefer minimal, local design changes that preserve existing APIs unless the task requires new API surface".to_string()
}

fn default_coding_style() -> String {
    "follow repository conventions and keep implementation scope focused".to_string()
}

fn default_review_priorities() -> String {
    "correctness, regressions, missing tests, maintainability".to_string()
}

fn default_ux_expectations() -> String {
    "preserve existing ergonomics and call out user-visible behavior changes".to_string()
}

fn default_ui_expectations() -> String {
    "match existing UI conventions and include visual review when UI changes are present"
        .to_string()
}

fn default_test_expectations() -> String {
    "run project-specific tests first, then broader gates when shared crates or protocols changed"
        .to_string()
}

fn stage_mode_schema() -> JsonValue {
    json!({
        "type": "string",
        "enum": ["auto", "on", "off"]
    })
}

fn string_schema(description: &str) -> JsonValue {
    json!({
        "type": "string",
        "description": description
    })
}

const STAGE_ALIASES: &[(&str, &str)] = &[
    ("stagePlanning", "planning"),
    ("stageArchitectureReview", "architectureReview"),
    ("stageImplementation", "implementation"),
    ("stageCodeReview", "codeReview"),
    ("stageUxReview", "uxReview"),
    ("stageUiReview", "uiReview"),
    ("stageTests", "tests"),
    ("stageFinalReview", "finalReview"),
    ("stageIntegration", "integration"),
];

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn stage_aliases_override_nested_stage_modes() {
        let (input, normalized) = parse_input(json!({
            "stages": {
                "tests": "on",
                "uiReview": "auto"
            },
            "stageTests": "off",
            "stageUiReview": "on"
        }))
        .unwrap();

        assert_eq!(input.stages["tests"], "off");
        assert_eq!(
            normalized,
            json!({
                "stages": {
                    "tests": "off",
                    "uiReview": "on"
                }
            })
        );
    }

    #[test]
    fn defaults_include_new_pipeline_settings() {
        let (input, _) = parse_input(default_input()).unwrap();

        assert_eq!(input.experiment_sample_rate, 0.20);
        assert_eq!(input.min_evidence_runs, 5);
        assert_eq!(input.max_rejected_grouping_experiments, 3);
        assert_eq!(input.max_review_items_per_group, 3);
        assert_eq!(input.baseline_resample_rate, 0.05);
        assert_eq!(input.test_mode, TestMode::Auto);
        assert!(input.effort_lowering_enabled);
        assert!(input.grouping_optimization_enabled);
    }

    #[test]
    fn baseline_resample_rate_must_be_probability() {
        let error = parse_input(json!({ "baselineResampleRate": 1.5 })).unwrap_err();

        assert_eq!(
            error.to_string(),
            "baselineResampleRate must be between 0.0 and 1.0"
        );
    }
}
