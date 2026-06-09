use anyhow::Result;
use codex_native_workflow::NativeWorkflow;
use codex_native_workflow::NativeWorkflowDefinition;
use codex_native_workflow::NativeWorkflowRunContext;
use codex_native_workflow::NativeWorkflowRunOutput;
use codex_native_workflow::NativeWorkflowStatusUpdate;
use codex_native_workflow::NativeWorkflowThreadStatus;
use serde_json::Map as JsonMap;
use serde_json::Value as JsonValue;
use serde_json::json;

pub const DEVELOPMENT_CYCLE_WORKFLOW_ID: &str = "dev-cycle";

#[derive(Debug, Clone, Copy, Default)]
pub struct DevelopmentCycleWorkflow;

pub fn workflow() -> DevelopmentCycleWorkflow {
    DevelopmentCycleWorkflow
}

impl NativeWorkflow for DevelopmentCycleWorkflow {
    fn definition(&self) -> NativeWorkflowDefinition {
        NativeWorkflowDefinition {
            id: DEVELOPMENT_CYCLE_WORKFLOW_ID.to_string(),
            command: Some("dev-cycle".to_string()),
            title: Some("Development Cycle Preview".to_string()),
            user_description: Some(
                "Preview the adaptive development-cycle plan, stage policy, and integration contract without launching agents or changing branches."
                    .to_string(),
            ),
            search_terms: vec![
                "development".to_string(),
                "implementation".to_string(),
                "review".to_string(),
                "testing".to_string(),
                "integration".to_string(),
            ],
            command_option_hints: Vec::new(),
            input_schema: Some(input_schema()),
            output_schema: Some(output_schema()),
            default_input: default_input(),
        }
    }

    async fn run(
        &self,
        ctx: NativeWorkflowRunContext<'_>,
        input: JsonValue,
    ) -> Result<NativeWorkflowRunOutput> {
        let input = normalize_stage_aliases(input);
        ctx.status(NativeWorkflowStatusUpdate {
            workflow_name: "dev-cycle".to_string(),
            workflow_status: "previewing adaptive development cycle".to_string(),
            threads: vec![NativeWorkflowThreadStatus {
                name: "planner".to_string(),
                status: "preparing stage plan and execution contract".to_string(),
            }],
            child_statuses: Vec::new(),
        });
        ctx.progress(
            "Prepared native development cycle preview",
            Some(json!({
                "workflowId": DEVELOPMENT_CYCLE_WORKFLOW_ID,
                "stage": "planning",
                "engine": "rust",
            })),
        );

        let stages = stage_plan(&input);
        let output = json!({
            "workflowId": DEVELOPMENT_CYCLE_WORKFLOW_ID,
            "engine": "rust",
            "status": "preview",
            "executionMode": "previewOnly",
            "workingDirectory": ctx.cwd.display().to_string(),
            "stagePlan": stages,
            "settings": input,
            "limitations": [
                "This native workflow currently prepares the development-cycle contract only.",
                "It does not launch agents, create git worktrees, make commits, cherry-pick changes, run tests, or update branches."
            ],
            "constraints": [
                "Future writer agents must work in isolated git worktrees.",
                "Future read-only agents should use read-only sandbox and tool policy.",
                "Future writable agents should be scoped to their assigned worktree.",
                "Future writer results must end as commits with clean git status.",
                "Future integration should cherry-pick accepted commits into a clean branch and run gates before updating the user branch."
            ],
        });
        let markdown = format_markdown(&output);
        if ctx.output_format == Some("tui.markdown.v1") {
            ctx.report_to_user_markdown(markdown.clone());
        }

        Ok(NativeWorkflowRunOutput {
            output,
            final_markdown: Some(markdown),
        })
    }
}

pub fn default_input() -> JsonValue {
    json!({
        "defaultStageMode": "auto",
        "maxParallelWriters": 2,
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
        "commitStyle": "small, reviewable commits with clear messages",
        "architectureStyle": "prefer minimal, local design changes that preserve existing APIs unless the task requires new API surface",
        "codingStyle": "follow repository conventions and keep implementation scope focused",
        "reviewPriorities": "correctness, regressions, missing tests, maintainability",
        "uxExpectations": "preserve existing ergonomics and call out user-visible behavior changes",
        "uiExpectations": "match existing UI conventions and include visual review when UI changes are present",
        "testExpectations": "run project-specific tests first, then broader gates when shared crates or protocols changed"
    })
}

fn input_schema() -> JsonValue {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "defaultStageMode": {
                "type": "string",
                "enum": ["auto", "on", "off"],
                "description": "Default mode applied to omitted stage settings."
            },
            "maxParallelWriters": {
                "type": "integer",
                "minimum": 1,
                "description": "Maximum number of writer agents that may run concurrently."
            },
            "integrationMode": {
                "type": "string",
                "enum": ["cherryPick", "manual"],
                "description": "How accepted writer commits are integrated."
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
            "stagePlanning": stage_alias_schema("Planning stage mode."),
            "stageArchitectureReview": stage_alias_schema("Architecture review stage mode."),
            "stageImplementation": stage_alias_schema("Implementation stage mode."),
            "stageCodeReview": stage_alias_schema("Code review stage mode."),
            "stageUxReview": stage_alias_schema("UX review stage mode."),
            "stageUiReview": stage_alias_schema("UI review stage mode."),
            "stageTests": stage_alias_schema("Test stage mode."),
            "stageFinalReview": stage_alias_schema("Final review stage mode."),
            "stageIntegration": stage_alias_schema("Integration stage mode."),
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

fn output_schema() -> JsonValue {
    json!({
        "type": "object",
        "required": ["workflowId", "engine", "status", "executionMode", "stagePlan", "settings", "constraints", "limitations"],
        "properties": {
            "workflowId": { "type": "string" },
            "engine": { "type": "string" },
            "status": { "type": "string" },
            "executionMode": { "type": "string" },
            "workingDirectory": { "type": "string" },
            "stagePlan": { "type": "array" },
            "settings": {
                "type": "object",
                "additionalProperties": true
            },
            "constraints": {
                "type": "array",
                "items": { "type": "string" }
            },
            "limitations": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

fn stage_mode_schema() -> JsonValue {
    json!({
        "type": "string",
        "enum": ["auto", "on", "off"]
    })
}

fn stage_alias_schema(description: &str) -> JsonValue {
    let mut schema = stage_mode_schema();
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "description".to_string(),
            JsonValue::String(description.to_string()),
        );
    }
    schema
}

fn string_schema(description: &str) -> JsonValue {
    json!({
        "type": "string",
        "description": description
    })
}

#[derive(Clone, Copy)]
struct StageDefinition {
    name: &'static str,
    alias_field: &'static str,
    description: &'static str,
}

const STAGES: [StageDefinition; 9] = [
    StageDefinition {
        name: "planning",
        alias_field: "stagePlanning",
        description: "Planner decides required and optional work for the task.",
    },
    StageDefinition {
        name: "architectureReview",
        alias_field: "stageArchitectureReview",
        description: "Architecture reviewer checks API boundaries and design risk.",
    },
    StageDefinition {
        name: "implementation",
        alias_field: "stageImplementation",
        description: "Writer agents implement scoped changes in isolated git worktrees.",
    },
    StageDefinition {
        name: "codeReview",
        alias_field: "stageCodeReview",
        description: "Code reviewer checks behavior, regressions, and test gaps.",
    },
    StageDefinition {
        name: "uxReview",
        alias_field: "stageUxReview",
        description: "UX reviewer checks workflow ergonomics when user-facing behavior changes.",
    },
    StageDefinition {
        name: "uiReview",
        alias_field: "stageUiReview",
        description: "UI reviewer checks layout and visual quality when UI changes are present.",
    },
    StageDefinition {
        name: "tests",
        alias_field: "stageTests",
        description: "Required gates run before integration is accepted.",
    },
    StageDefinition {
        name: "finalReview",
        alias_field: "stageFinalReview",
        description: "Final reviewer checks integrated state before handoff.",
    },
    StageDefinition {
        name: "integration",
        alias_field: "stageIntegration",
        description: "Integrator cherry-picks accepted commits and updates the user branch after gates pass.",
    },
];

fn normalize_stage_aliases(mut input: JsonValue) -> JsonValue {
    let JsonValue::Object(input_object) = &mut input else {
        return input;
    };
    let mut stage_overrides = JsonMap::new();
    for stage in STAGES {
        if let Some(value) = input_object.remove(stage.alias_field) {
            stage_overrides.insert(stage.name.to_string(), value);
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

fn stage_plan(input: &JsonValue) -> Vec<JsonValue> {
    let default_mode = input
        .get("defaultStageMode")
        .and_then(JsonValue::as_str)
        .unwrap_or("auto");
    let stage_modes = input.get("stages").and_then(JsonValue::as_object);

    STAGES
        .into_iter()
        .map(|stage| {
            json!({
                "name": stage.name,
                "mode": stage_modes
                    .and_then(|stages| stages.get(stage.name))
                    .and_then(JsonValue::as_str)
                    .unwrap_or(default_mode),
                "description": stage.description,
            })
        })
        .collect()
}

fn format_markdown(output: &JsonValue) -> String {
    let mut lines = vec![
        "# Development Cycle Preview".to_string(),
        String::new(),
        "Native Rust workflow runtime prepared a development-cycle preview. This preview does not launch agents, create worktrees, make commits, cherry-pick changes, run tests, or update branches.".to_string(),
        String::new(),
        "## Stage Plan".to_string(),
    ];

    for stage in output
        .get("stagePlan")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
    {
        let name = stage
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or("stage");
        let mode = stage
            .get("mode")
            .and_then(JsonValue::as_str)
            .unwrap_or("auto");
        let description = stage
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        lines.push(format!("- `{name}`: `{mode}` - {description}"));
    }

    lines.extend([String::new(), "## Limitations".to_string()]);
    for limitation in output
        .get("limitations")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
    {
        lines.push(format!("- {limitation}"));
    }

    lines.extend([String::new(), "## Constraints".to_string()]);
    for constraint in output
        .get("constraints")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
    {
        lines.push(format!("- {constraint}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use codex_native_workflow::NativeWorkflow;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::DevelopmentCycleWorkflow;
    use super::default_input;
    use super::format_markdown;
    use super::normalize_stage_aliases;

    #[test]
    fn definition_exposes_dev_cycle_metadata_and_schema() {
        let definition = DevelopmentCycleWorkflow.definition();

        assert_eq!(definition.id, "dev-cycle");
        assert_eq!(definition.command.as_deref(), Some("dev-cycle"));
        assert!(definition.input_schema.is_some());
        assert!(definition.output_schema.is_some());
    }

    #[test]
    fn defaults_include_adaptive_stage_plan() {
        assert_eq!(default_input()["stages"]["integration"], json!("on"));
        assert_eq!(default_input()["defaultStageMode"], json!("auto"));
    }

    #[test]
    fn stage_aliases_override_nested_stage_modes() {
        assert_eq!(
            normalize_stage_aliases(json!({
                "stages": {
                    "tests": "on",
                    "uiReview": "auto"
                },
                "stageTests": "off",
                "stageUiReview": "on"
            })),
            json!({
                "stages": {
                    "tests": "off",
                    "uiReview": "on"
                }
            })
        );
    }

    #[test]
    fn markdown_makes_preview_limitation_explicit() {
        let markdown = format_markdown(&json!({
            "stagePlan": [],
            "limitations": [
                "This native workflow currently prepares the development-cycle contract only."
            ],
            "constraints": []
        }));

        assert!(markdown.contains("# Development Cycle Preview"));
        assert!(markdown.contains("does not launch agents"));
    }
}
