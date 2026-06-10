use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

/// Defines the contract for a compiled-in Rust workflow.
///
/// Implementations provide static metadata used for discovery and a `run`
/// method that receives normalized JSON input. Hosts are expected to validate
/// the input/output schemas, forward progress events, and expose the workflow
/// through the same public surface as other workflow engines.
pub trait NativeWorkflow: Send + Sync {
    fn definition(&self) -> NativeWorkflowDefinition;

    fn run(
        &self,
        ctx: NativeWorkflowRunContext<'_>,
        input: JsonValue,
    ) -> impl std::future::Future<Output = Result<NativeWorkflowRunOutput>> + Send;

    fn complete(
        &self,
        _request: NativeWorkflowCompletionRequest,
    ) -> impl std::future::Future<Output = Result<Vec<NativeWorkflowCompletionSuggestion>>> + Send
    {
        async { Ok(Vec::new()) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowCommandOptionHint {
    pub display: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowDefinition {
    pub id: String,
    pub command: Option<String>,
    pub title: Option<String>,
    pub user_description: Option<String>,
    pub search_terms: Vec<String>,
    pub command_option_hints: Vec<NativeWorkflowCommandOptionHint>,
    pub input_schema: Option<JsonValue>,
    pub output_schema: Option<JsonValue>,
    pub default_input: JsonValue,
}

#[derive(Clone)]
pub struct NativeWorkflowRunContext<'a> {
    pub codex_home: &'a Path,
    pub cwd: &'a Path,
    pub state_dir: &'a Path,
    pub output_format: Option<&'a str>,
    pub event_handler: Option<&'a NativeWorkflowEventHandler<'a>>,
    pub agent_runtime: Option<&'a dyn NativeWorkflowAgentRuntime>,
    pub model_provider_catalog: Option<&'a dyn NativeWorkflowModelProviderCatalog>,
    pub cancellation_token: Option<&'a dyn NativeWorkflowCancellation>,
}

impl NativeWorkflowRunContext<'_> {
    pub fn progress(&self, message: impl Into<String>, data: Option<JsonValue>) {
        self.emit(NativeWorkflowEvent::Progress {
            message: message.into(),
            data,
        });
    }

    pub fn status(&self, status: NativeWorkflowStatusUpdate) {
        self.emit(NativeWorkflowEvent::Status { status });
    }

    pub fn report_to_user_markdown(&self, markdown: impl Into<String>) {
        self.emit(NativeWorkflowEvent::ReportToUserMarkdown {
            markdown: markdown.into(),
        });
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation_token
            .is_some_and(NativeWorkflowCancellation::is_cancelled)
    }

    pub fn ensure_not_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            anyhow::bail!("native workflow run canceled");
        }
        Ok(())
    }

    fn emit(&self, event: NativeWorkflowEvent) {
        if let Some(event_handler) = self.event_handler {
            event_handler(&event);
        }
    }
}

pub type NativeWorkflowEventHandler<'a> = dyn Fn(&NativeWorkflowEvent) + Send + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NativeWorkflowEvent {
    #[serde(rename = "status")]
    Status { status: NativeWorkflowStatusUpdate },
    #[serde(rename = "progress")]
    Progress {
        message: String,
        data: Option<JsonValue>,
    },
    #[serde(rename = "reportToUserMarkdown")]
    ReportToUserMarkdown { markdown: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowThreadStatus {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowChildStatus {
    pub workflow_name: String,
    pub workflow_status: String,
    #[serde(default)]
    pub threads: Vec<NativeWorkflowThreadStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowStatusUpdate {
    pub workflow_name: String,
    pub workflow_status: String,
    #[serde(default)]
    pub threads: Vec<NativeWorkflowThreadStatus>,
    #[serde(default)]
    pub child_statuses: Vec<NativeWorkflowChildStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowRunOutput {
    pub output: JsonValue,
    pub final_markdown: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowModelCandidate {
    pub provider_id: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub intelligence_score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowModelSelection {
    pub provider_id: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowAgentSpawnRequest {
    pub name: String,
    pub role: String,
    pub prompt: String,
    pub cwd: PathBuf,
    pub writable: bool,
    pub model: Option<NativeWorkflowModelSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowAgentHandle {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowAgentTurnRequest {
    pub agent_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowAgentOutput {
    pub agent_id: String,
    pub text: String,
    #[serde(default)]
    pub metadata: JsonValue,
}

/// Agent host service used by native workflows to run isolated agent threads.
///
/// Implementations own thread creation, follow-up routing, sandbox/tool policy,
/// and the final-output contract. Workflows should treat handles as opaque and
/// send follow-up work back to the same handle when continuity is required.
pub trait NativeWorkflowAgentRuntime: Send + Sync {
    fn spawn_agent(
        &self,
        request: NativeWorkflowAgentSpawnRequest,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<NativeWorkflowAgentHandle>> + Send + '_>>;

    fn send_follow_up(
        &self,
        request: NativeWorkflowAgentTurnRequest,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;

    fn wait_for_output(
        &self,
        agent_id: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<NativeWorkflowAgentOutput>> + Send + '_>>;
}

/// Supplies model candidates that native workflows may use for internal agents.
///
/// Hosts should return candidates already constrained to models/providers that
/// are usable for the current session. Workflows may sort and sample from that
/// list based on their own experiment policy.
pub trait NativeWorkflowModelProviderCatalog: Send + Sync {
    fn model_candidates(&self) -> Vec<NativeWorkflowModelCandidate>;
}

/// Cancellation signal for native workflow orchestration.
///
/// Implementations should be cheap to poll and safe to call frequently between
/// agent and I/O operations.
pub trait NativeWorkflowCancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowCompletionRequest {
    pub input: JsonValue,
    pub active_field: Option<String>,
    pub prefix: String,
    pub mode: NativeWorkflowCompletionMode,
    pub replacement_prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeWorkflowCompletionMode {
    Field,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkflowCompletionSuggestion {
    pub display: String,
    pub insert_text: String,
    pub description: Option<String>,
}
