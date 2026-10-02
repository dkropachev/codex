use anyhow::bail;
use serde::Serialize;

pub const WORKFLOW_OUTPUT_MIN_BYTES: usize = 64;
pub const WORKFLOW_OUTPUT_MAX_BYTES: usize = 32 * 1024;

/// Trusted, per-run settings for executing and validating a workflow.
///
/// The same value must be supplied to the embedded runner and to completion-frame parsing so both
/// sides enforce an identical bound on final `markdown.v1` output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunConfig {
    output_max_bytes: usize,
}

impl WorkflowRunConfig {
    pub fn new(output_max_bytes: usize) -> anyhow::Result<Self> {
        if !(WORKFLOW_OUTPUT_MIN_BYTES..=WORKFLOW_OUTPUT_MAX_BYTES).contains(&output_max_bytes) {
            bail!(
                "workflow output limit must be between {WORKFLOW_OUTPUT_MIN_BYTES} and {WORKFLOW_OUTPUT_MAX_BYTES} bytes"
            );
        }
        Ok(Self { output_max_bytes })
    }

    pub const fn output_max_bytes(self) -> usize {
        self.output_max_bytes
    }
}

impl Default for WorkflowRunConfig {
    fn default() -> Self {
        Self {
            output_max_bytes: WORKFLOW_OUTPUT_MAX_BYTES,
        }
    }
}
