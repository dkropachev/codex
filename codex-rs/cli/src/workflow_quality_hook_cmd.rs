use std::io::Read;
use std::path::PathBuf;

use anyhow::Context as _;
use anyhow::Result;
use codex_config::types::WorkflowsConfigToml;
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowQualityHookInput {
    cwd: String,
}

struct WorkflowQualityHookContext {
    codex_home: PathBuf,
    workflows_config: WorkflowsConfigToml,
    input: WorkflowQualityHookInput,
}

pub(crate) fn run_workflow_quality_hook() -> Result<()> {
    match workflow_quality_feedback() {
        Ok(Some(feedback)) => {
            println!(
                "{}",
                json!({
                    "decision": "block",
                    "reason": feedback.reason,
                    "hookSpecificOutput": {
                        "hookEventName": "PostToolUse",
                        "additionalContext": feedback.additional_context,
                    }
                })
            );
        }
        Ok(None) => {}
        Err(err) => {
            println!(
                "{}",
                json!({
                    "decision": "block",
                    "reason": format!("workflow quality hook failed: {err}")
                })
            );
        }
    }
    Ok(())
}

fn workflow_quality_feedback() -> Result<Option<codex_workflows::WorkflowQualityHookFeedback>> {
    let context = load_context()?;
    let cwd = PathBuf::from(context.input.cwd);
    codex_workflows::workflow_quality_feedback(
        context.codex_home.as_path(),
        cwd.as_path(),
        &context.workflows_config,
    )
}

fn load_context() -> Result<WorkflowQualityHookContext> {
    let input = read_input()?;
    let codex_home = std::env::var("CODEX_WORKFLOW_QUALITY_HOME")
        .context("missing CODEX_WORKFLOW_QUALITY_HOME env var")?;
    let workflows_config_json = std::env::var("CODEX_WORKFLOW_QUALITY_WORKFLOWS_CONFIG")
        .context("missing CODEX_WORKFLOW_QUALITY_WORKFLOWS_CONFIG env var")?;
    let workflows_config = serde_json::from_str::<WorkflowsConfigToml>(&workflows_config_json)
        .context("failed to parse CODEX_WORKFLOW_QUALITY_WORKFLOWS_CONFIG")?;

    Ok(WorkflowQualityHookContext {
        codex_home: PathBuf::from(codex_home),
        workflows_config,
        input,
    })
}

fn read_input() -> Result<WorkflowQualityHookInput> {
    let mut stdin = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin)
        .context("failed to read workflow quality hook stdin")?;
    serde_json::from_str(&stdin).context("failed to parse workflow quality hook stdin")
}
