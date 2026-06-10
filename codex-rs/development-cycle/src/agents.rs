use std::path::Path;
use std::path::PathBuf;

use codex_native_workflow::NativeWorkflowAgentHandle;
use codex_native_workflow::NativeWorkflowAgentRuntime;
use codex_native_workflow::NativeWorkflowAgentSpawnRequest;
use codex_native_workflow::NativeWorkflowModelSelection;

use crate::models::model_score_key;
use crate::persistence::AgentAttemptRecord;
use crate::persistence::DevCycleState;

pub(crate) struct AgentExecutionContext<'a> {
    pub(crate) runtime: &'a dyn NativeWorkflowAgentRuntime,
    pub(crate) state: &'a DevCycleState,
    pub(crate) run_id: &'a str,
    pub(crate) cwd: &'a Path,
}

pub(crate) struct AgentSpawnSpec {
    pub(crate) role: &'static str,
    pub(crate) name: String,
    pub(crate) prompt: String,
    pub(crate) cwd: PathBuf,
    pub(crate) writable: bool,
    pub(crate) model: Option<NativeWorkflowModelSelection>,
}

pub(crate) async fn spawn_agent(
    agents: &AgentExecutionContext<'_>,
    spec: AgentSpawnSpec,
) -> anyhow::Result<NativeWorkflowAgentHandle> {
    let model_key = spec.model.as_ref().map(model_score_key);
    let handle = agents
        .runtime
        .spawn_agent(NativeWorkflowAgentSpawnRequest {
            name: spec.name.clone(),
            role: spec.role.to_string(),
            prompt: spec.prompt.clone(),
            cwd: spec.cwd,
            writable: spec.writable,
            model: spec.model,
        })
        .await?;
    agents.state.record_agent_attempt(AgentAttemptRecord {
        run_id: agents.run_id,
        role: spec.role,
        name: &spec.name,
        agent_id: Some(&handle.id),
        model_key: model_key.as_deref(),
        status: "spawned",
        prompt: &spec.prompt,
        output_json: None,
    })?;
    Ok(handle)
}
