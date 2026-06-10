use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context;
use anyhow::Result;
use codex_config::types::WorkflowsConfigToml;
use codex_native_workflow::NativeWorkflow;
use codex_native_workflow::NativeWorkflowCancellation;
use codex_native_workflow::NativeWorkflowCompletionMode;
use codex_native_workflow::NativeWorkflowCompletionRequest;
use codex_native_workflow::NativeWorkflowDefinition;
use codex_native_workflow::NativeWorkflowEvent;
use codex_native_workflow::NativeWorkflowModelCandidate;
use codex_native_workflow::NativeWorkflowModelProviderCatalog;
use codex_native_workflow::NativeWorkflowRunContext;
use codex_native_workflow::NativeWorkflowRunOutput;
use codex_native_workflow::NativeWorkflowStatusUpdate;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::command_completion::command_option_hints_from_input_schema;
use crate::id::mention_target;
use crate::input_adapter::WorkflowCompletionMode;
use crate::registry::WorkflowEngine;
use crate::registry::WorkflowRootKind;
use crate::registry::WorkflowSummary;
use crate::registry::WorkflowValidation;
use crate::workflow_runtime::WorkflowRuntimeEvent;
use crate::workflow_runtime::WorkflowRuntimeEventHandler;
use crate::workflow_runtime::WorkflowRuntimeOutput;
use crate::workflow_runtime::WorkflowStatusUpdate;

pub(crate) const NATIVE_ROOT_LABEL: &str = "native";
const NATIVE_ROOT_DIR: &str = ".native-workflows";

pub(crate) fn native_workflow_definitions() -> Vec<NativeWorkflowDefinition> {
    vec![codex_development_cycle::workflow().definition()]
}

pub(crate) fn discover_native_workflows(
    codex_home: &Path,
    config: &WorkflowsConfigToml,
) -> Result<Vec<WorkflowSummary>> {
    if !rust_engine_enabled(config) {
        return Ok(Vec::new());
    }

    native_workflow_definitions()
        .into_iter()
        .filter(|definition| workflow_enabled(config, &definition.id))
        .map(|definition| summarize_native_workflow(codex_home, definition))
        .collect()
}

pub fn native_workflow_spec_yaml(id: &str) -> Result<String> {
    let definition = native_definition(id)?;
    serde_yaml::to_string(&json!({
        "id": definition.id,
        "command": definition.command,
        "title": definition.title,
        "userDescription": definition.user_description,
        "searchTerms": definition.search_terms,
        "engine": "rust",
        "api": {
            "inputSchema": definition.input_schema,
            "outputSchema": definition.output_schema,
        },
        "defaults": definition.default_input,
    }))
    .context("failed to serialize native workflow metadata")
}

pub(crate) fn native_default_input(config: &WorkflowsConfigToml, id: &str) -> Result<JsonValue> {
    let definition = native_definition(id)?;
    let mut value = JsonValue::Object(Default::default());
    if let Some(default_input) = config
        .engines
        .as_ref()
        .and_then(|engines| engines.rust.as_ref())
        .and_then(|rust| rust.default_input.clone())
    {
        merge_json(&mut value, default_input);
    }
    merge_json(&mut value, definition.default_input);
    if let Some(default_input) = config
        .workflow_overrides
        .get(id)
        .and_then(|override_config| override_config.default_input.clone())
    {
        merge_json(&mut value, default_input);
    }
    Ok(value)
}

pub(crate) async fn run_native_workflow(
    codex_home: &Path,
    cwd: &Path,
    id: &str,
    input: &str,
    output_format: Option<&str>,
    runtime: &crate::execute::WorkflowRuntimeContext,
    event_handler: Option<&WorkflowRuntimeEventHandler<'_>>,
) -> Result<WorkflowRuntimeOutput> {
    let definition = native_definition(id)?;
    let input = serde_json::from_str::<JsonValue>(input)
        .with_context(|| format!("failed to parse input for native workflow {id}"))?;
    let state_dir = native_root_path(codex_home).join(id).join("state");
    std::fs::create_dir_all(&state_dir).with_context(|| {
        format!(
            "failed to create native workflow state directory {}",
            state_dir.display()
        )
    })?;
    let event_handler = |event: &NativeWorkflowEvent| {
        if let Some(event_handler) = event_handler {
            event_handler(&native_event_to_runtime(event.clone()));
        }
    };
    let model_catalog = RuntimeModelProviderCatalog {
        candidates: runtime.model_candidates.clone(),
    };
    let cancellation = runtime
        .cancellation_flag
        .as_ref()
        .map(|flag| RuntimeCancellation {
            flag: Arc::clone(flag),
        });
    let ctx = NativeWorkflowRunContext {
        codex_home,
        cwd,
        state_dir: &state_dir,
        output_format,
        event_handler: Some(&event_handler),
        agent_runtime: runtime
            .native_agent_runtime
            .as_deref()
            .map(|runtime| runtime as &dyn codex_native_workflow::NativeWorkflowAgentRuntime),
        model_provider_catalog: Some(&model_catalog),
        cancellation_token: cancellation
            .as_ref()
            .map(|token| token as &dyn NativeWorkflowCancellation),
    };
    let output = match id {
        codex_development_cycle::DEVELOPMENT_CYCLE_WORKFLOW_ID => {
            codex_development_cycle::workflow().run(ctx, input).await?
        }
        _ => anyhow::bail!("unknown native workflow '{id}'"),
    };
    if let Some(output_schema) = definition.output_schema.as_ref() {
        crate::workflow_contract_validation::validate_json_against_schema(
            output_schema,
            &output.output,
        )
        .with_context(|| format!("native workflow output for {id} did not match schema"))?;
    }
    native_output_to_runtime(output)
}

struct RuntimeModelProviderCatalog {
    candidates: Vec<NativeWorkflowModelCandidate>,
}

impl NativeWorkflowModelProviderCatalog for RuntimeModelProviderCatalog {
    fn model_candidates(&self) -> Vec<NativeWorkflowModelCandidate> {
        self.candidates.clone()
    }
}

struct RuntimeCancellation {
    flag: Arc<AtomicBool>,
}

impl NativeWorkflowCancellation for RuntimeCancellation {
    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

pub(crate) async fn complete_native_workflow(
    id: &str,
    input: &crate::WorkflowCommandInput,
) -> Result<Vec<crate::command_completion::WorkflowCommandCompletionSuggestion>> {
    let request = NativeWorkflowCompletionRequest {
        input: input.input.clone(),
        active_field: input.active_field.clone(),
        prefix: input.prefix.clone(),
        mode: match input.mode {
            WorkflowCompletionMode::Field => NativeWorkflowCompletionMode::Field,
            WorkflowCompletionMode::Value => NativeWorkflowCompletionMode::Value,
        },
        replacement_prefix: input.replacement_prefix.clone(),
    };
    let suggestions = match id {
        codex_development_cycle::DEVELOPMENT_CYCLE_WORKFLOW_ID => {
            codex_development_cycle::workflow()
                .complete(request)
                .await?
        }
        _ => anyhow::bail!("unknown native workflow '{id}'"),
    };
    Ok(suggestions
        .into_iter()
        .map(
            |suggestion| crate::command_completion::WorkflowCommandCompletionSuggestion {
                display: suggestion.display,
                insert_text: suggestion.insert_text,
                description: suggestion.description,
            },
        )
        .collect())
}

fn summarize_native_workflow(
    codex_home: &Path,
    definition: NativeWorkflowDefinition,
) -> Result<WorkflowSummary> {
    let root_path = native_root_path(codex_home);
    let path = root_path.join(&definition.id);
    let mention_target = mention_target(&root_path, &definition.id)?;
    let command_option_hints = if definition.command_option_hints.is_empty() {
        command_option_hints_from_input_schema(definition.input_schema.as_ref())
    } else {
        definition
            .command_option_hints
            .into_iter()
            .map(
                |hint| crate::command_completion::WorkflowCommandOptionHint {
                    display: hint.display,
                    description: hint.description,
                },
            )
            .collect()
    };

    Ok(WorkflowSummary {
        id: definition.id,
        engine: WorkflowEngine::Rust,
        command: definition.command,
        title: definition.title,
        user_description: definition.user_description,
        search_terms: definition.search_terms,
        command_option_hints,
        input_schema: definition.input_schema,
        root_label: NATIVE_ROOT_LABEL.to_string(),
        root_kind: WorkflowRootKind::Global,
        root_path,
        path: path.clone(),
        workflow_yaml_path: path.join(crate::spec::WORKFLOW_YAML),
        mention_target,
        validation: WorkflowValidation::valid(),
        repair_mode: crate::registry::DEFAULT_REPAIR_MODE.to_string(),
    })
}

fn native_root_path(codex_home: &Path) -> PathBuf {
    codex_home.join("workflows").join(NATIVE_ROOT_DIR)
}

fn rust_engine_enabled(config: &WorkflowsConfigToml) -> bool {
    match config
        .engines
        .as_ref()
        .and_then(|engines| engines.rust.as_ref())
        .and_then(|rust| rust.enabled)
    {
        Some(false) => false,
        Some(true) => true,
        None => config
            .workflow_overrides
            .values()
            .any(|override_config| override_config.enabled == Some(true)),
    }
}

fn workflow_enabled(config: &WorkflowsConfigToml, id: &str) -> bool {
    config
        .workflow_overrides
        .get(id)
        .and_then(|override_config| override_config.enabled)
        .unwrap_or(true)
}

fn native_definition(id: &str) -> Result<NativeWorkflowDefinition> {
    native_workflow_definitions()
        .into_iter()
        .find(|definition| definition.id == id)
        .ok_or_else(|| anyhow::anyhow!("unknown native workflow '{id}'"))
}

fn merge_json(base: &mut JsonValue, overlay: JsonValue) {
    match (base, overlay) {
        (JsonValue::Object(base), JsonValue::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(base_value) => merge_json(base_value, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn native_event_to_runtime(event: NativeWorkflowEvent) -> WorkflowRuntimeEvent {
    match event {
        NativeWorkflowEvent::Status { status } => WorkflowRuntimeEvent::Status {
            status: native_status_to_runtime(status),
        },
        NativeWorkflowEvent::Progress { message, data } => {
            WorkflowRuntimeEvent::Progress { message, data }
        }
        NativeWorkflowEvent::ReportToUserMarkdown { markdown } => {
            WorkflowRuntimeEvent::ReportToUserMarkdown { markdown }
        }
    }
}

fn native_status_to_runtime(status: NativeWorkflowStatusUpdate) -> WorkflowStatusUpdate {
    WorkflowStatusUpdate {
        workflow_name: status.workflow_name,
        workflow_status: status.workflow_status,
        threads: status
            .threads
            .into_iter()
            .map(|thread| crate::workflow_runtime::WorkflowThreadStatus {
                name: thread.name,
                status: thread.status,
            })
            .collect(),
        child_statuses: status
            .child_statuses
            .into_iter()
            .map(|child| crate::workflow_runtime::WorkflowChildStatus {
                workflow_name: child.workflow_name,
                workflow_status: child.workflow_status,
                threads: child
                    .threads
                    .into_iter()
                    .map(|thread| crate::workflow_runtime::WorkflowThreadStatus {
                        name: thread.name,
                        status: thread.status,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn native_output_to_runtime(output: NativeWorkflowRunOutput) -> Result<WorkflowRuntimeOutput> {
    Ok(WorkflowRuntimeOutput {
        stdout: serde_json::to_string_pretty(&output.output)?,
        stderr: String::new(),
        success: true,
        exit_status: "exit status: 0".to_string(),
        final_markdown: output.final_markdown,
    })
}

#[cfg(test)]
mod tests {
    use codex_config::types::RustWorkflowEngineConfigToml;
    use codex_config::types::WorkflowEnginesConfigToml;
    use codex_config::types::WorkflowOverrideConfigToml;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn native_workflows_are_discovered_when_rust_engine_is_enabled() {
        let home = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            engines: Some(WorkflowEnginesConfigToml {
                rust: Some(RustWorkflowEngineConfigToml {
                    enabled: Some(true),
                    default_input: None,
                }),
            }),
            ..Default::default()
        };

        let workflows = discover_native_workflows(home.path(), &config).unwrap();

        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].id, "dev-cycle");
        assert_eq!(workflows[0].engine, WorkflowEngine::Rust);
    }

    #[test]
    fn native_workflows_are_hidden_when_rust_engine_is_disabled() {
        let home = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            engines: Some(WorkflowEnginesConfigToml {
                rust: Some(RustWorkflowEngineConfigToml {
                    enabled: Some(false),
                    default_input: None,
                }),
            }),
            workflow_overrides: [(
                "dev-cycle".to_string(),
                WorkflowOverrideConfigToml {
                    enabled: Some(true),
                    default_input: None,
                },
            )]
            .into(),
            ..Default::default()
        };

        assert_eq!(
            discover_native_workflows(home.path(), &config).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn workflow_override_can_enable_native_workflow_without_engine_config() {
        let home = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            workflow_overrides: [(
                "dev-cycle".to_string(),
                WorkflowOverrideConfigToml {
                    enabled: Some(true),
                    default_input: None,
                },
            )]
            .into(),
            ..Default::default()
        };

        let workflows = discover_native_workflows(home.path(), &config).unwrap();

        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].id, "dev-cycle");
    }

    #[test]
    fn workflow_override_can_disable_single_native_workflow() {
        let home = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            engines: Some(WorkflowEnginesConfigToml {
                rust: Some(RustWorkflowEngineConfigToml {
                    enabled: Some(true),
                    default_input: None,
                }),
            }),
            workflow_overrides: [(
                "dev-cycle".to_string(),
                WorkflowOverrideConfigToml {
                    enabled: Some(false),
                    default_input: None,
                },
            )]
            .into(),
            ..Default::default()
        };

        assert_eq!(
            discover_native_workflows(home.path(), &config).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn native_summary_exposes_stage_option_hints() {
        let home = TempDir::new().unwrap();
        let config = WorkflowsConfigToml {
            engines: Some(WorkflowEnginesConfigToml {
                rust: Some(RustWorkflowEngineConfigToml {
                    enabled: Some(true),
                    default_input: None,
                }),
            }),
            ..Default::default()
        };

        let workflows = discover_native_workflows(home.path(), &config).unwrap();

        let displays = workflows[0]
            .command_option_hints
            .iter()
            .map(|hint| hint.display.as_str())
            .collect::<Vec<_>>();
        assert!(displays.contains(&"--stage-tests <auto|on|off>"));
        assert!(displays.contains(&"--stage-integration <auto|on|off>"));
    }

    #[test]
    fn native_default_input_merges_engine_workflow_and_override_defaults() {
        let config = WorkflowsConfigToml {
            engines: Some(WorkflowEnginesConfigToml {
                rust: Some(RustWorkflowEngineConfigToml {
                    enabled: Some(true),
                    default_input: Some(json!({
                        "maxParallelWriters": 4,
                        "stages": { "uiReview": "off" }
                    })),
                }),
            }),
            workflow_overrides: [(
                "dev-cycle".to_string(),
                WorkflowOverrideConfigToml {
                    enabled: None,
                    default_input: Some(json!({
                        "stages": { "uiReview": "on" },
                        "integrationMode": "manual"
                    })),
                },
            )]
            .into(),
            ..Default::default()
        };

        let default_input = native_default_input(&config, "dev-cycle").unwrap();

        assert_eq!(default_input["maxParallelWriters"], json!(2));
        assert_eq!(default_input["integrationMode"], json!("manual"));
        assert_eq!(default_input["stages"]["uiReview"], json!("on"));
    }
}
