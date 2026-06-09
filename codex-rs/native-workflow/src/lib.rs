use std::path::Path;

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
    pub cwd: &'a Path,
    pub output_format: Option<&'a str>,
    pub event_handler: Option<&'a NativeWorkflowEventHandler<'a>>,
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
