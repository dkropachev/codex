mod agents;
mod execution;
mod experiment;
mod input;
mod models;
mod output;
mod persistence;
mod pipeline;
#[cfg(test)]
mod pipeline_tests;
mod review_split;
mod review_stage;
mod review_types;
mod split_persistence;
mod work_size;

use anyhow::Result;
use codex_native_workflow::NativeWorkflow;
use codex_native_workflow::NativeWorkflowCommandOptionHint;
use codex_native_workflow::NativeWorkflowCompletionMode;
use codex_native_workflow::NativeWorkflowCompletionRequest;
use codex_native_workflow::NativeWorkflowCompletionSuggestion;
use codex_native_workflow::NativeWorkflowDefinition;
use codex_native_workflow::NativeWorkflowRunContext;
use codex_native_workflow::NativeWorkflowRunOutput;
use serde_json::Value as JsonValue;

use crate::input::default_input;
use crate::input::input_schema;
use crate::input::output_schema;
use crate::pipeline::run_dev_cycle;
use crate::review_types::review_type_suggestions;

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
            title: Some("Development Cycle".to_string()),
            user_description: Some(
                "Run the native fix/review/verify/test/integrate development-cycle workflow."
                    .to_string(),
            ),
            search_terms: vec![
                "development".to_string(),
                "implementation".to_string(),
                "review".to_string(),
                "verification".to_string(),
                "testing".to_string(),
                "integration".to_string(),
            ],
            command_option_hints: vec![
                NativeWorkflowCommandOptionHint {
                    display: "--task-description <text>".to_string(),
                    description: Some("Task for the planner and writer agents.".to_string()),
                },
                NativeWorkflowCommandOptionHint {
                    display: "--review-types <id>".to_string(),
                    description: Some(
                        "Review type id such as correctness, security, tests, ui, or docs."
                            .to_string(),
                    ),
                },
                NativeWorkflowCommandOptionHint {
                    display: "--test-mode <auto|provided|off>".to_string(),
                    description: Some("How dev-cycle should run test gates.".to_string()),
                },
                NativeWorkflowCommandOptionHint {
                    display: "--stage-tests <auto|on|off>".to_string(),
                    description: Some("Test stage mode.".to_string()),
                },
                NativeWorkflowCommandOptionHint {
                    display: "--stage-integration <auto|on|off>".to_string(),
                    description: Some("Integration stage mode.".to_string()),
                },
            ],
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
        run_dev_cycle(ctx, input).await
    }

    async fn complete(
        &self,
        request: NativeWorkflowCompletionRequest,
    ) -> Result<Vec<NativeWorkflowCompletionSuggestion>> {
        match (request.active_field.as_deref(), request.mode) {
            (Some("reviewTypes"), NativeWorkflowCompletionMode::Value) => {
                review_type_suggestions(&request.input, &request.prefix)
            }
            _ => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use codex_native_workflow::NativeWorkflow;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn definition_exposes_dev_cycle_pipeline_metadata_and_schema() {
        let definition = DevelopmentCycleWorkflow.definition();

        assert_eq!(definition.id, "dev-cycle");
        assert_eq!(definition.command.as_deref(), Some("dev-cycle"));
        assert_eq!(definition.title.as_deref(), Some("Development Cycle"));
        assert!(definition.input_schema.is_some());
        assert!(definition.output_schema.is_some());
    }

    #[tokio::test]
    async fn completion_suggests_review_type_ids() {
        let suggestions = DevelopmentCycleWorkflow
            .complete(NativeWorkflowCompletionRequest {
                input: json!({}),
                active_field: Some("reviewTypes".to_string()),
                prefix: "sec".to_string(),
                mode: NativeWorkflowCompletionMode::Value,
                replacement_prefix: "sec".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(
            suggestions,
            vec![NativeWorkflowCompletionSuggestion {
                display: "security".to_string(),
                insert_text: "security".to_string(),
                description: Some(
                    "Security: auth, injection, secrets, unsafe permissions, data exposure"
                        .to_string()
                ),
            }]
        );
    }
}
