use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRepairActionKind {
    NormalizeValidationMetadata,
    RepairReadme,
    RepairDesign,
    RepairLayout,
    RepairPackageManifest,
    RepairTsconfig,
    ScaffoldWorkflowSource,
    ScaffoldWorkflowTests,
    AddCoverageMarkers,
    AiRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRepairAction {
    pub kind: WorkflowRepairActionKind,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowRepairStopReason {
    Valid,
    BlockedByRepairMode,
    UnsupportedFindings,
    RepairBudgetExhausted,
    NoChangesApplied,
}

pub type WorkflowValidationFindingInfo = crate::validation_finding::WorkflowValidationFinding;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowValidationInfo {
    pub status: crate::registry::WorkflowValidationStatus,
    pub findings: Vec<WorkflowValidationFindingInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRepairResult {
    pub mode: String,
    pub max_repair_cycles: u32,
    pub repair_cycles_run: u32,
    pub changed: bool,
    pub stop_reason: WorkflowRepairStopReason,
    pub applied_fixes: Vec<WorkflowRepairAction>,
    pub remaining_findings: Vec<WorkflowValidationFindingInfo>,
    pub blocked_findings: Vec<WorkflowValidationFindingInfo>,
    pub unsupported_findings: Vec<WorkflowValidationFindingInfo>,
}
