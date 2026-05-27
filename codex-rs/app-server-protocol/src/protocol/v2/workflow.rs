use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRootKind {
    Global,
    Project,
    SearchPath,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowValidationStatus {
    Valid,
    Invalid,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", export_to = "v2/")]
pub enum WorkflowValidationFindingInfo {
    WorkflowSpecReadFailed {
        path: PathBuf,
        error: String,
    },
    WorkflowIdMismatch {
        path: PathBuf,
        expected_id: String,
        actual_id: String,
    },
    MissingFile {
        path: PathBuf,
    },
    MissingDirectory {
        path: PathBuf,
    },
    MissingGitRepository {
        path: PathBuf,
    },
    WorkflowPathEscapesRoot {
        workflow_path: PathBuf,
        root_path: PathBuf,
    },
    MissingDocumentHeading {
        path: PathBuf,
        heading: String,
    },
    EmptyDocumentSection {
        path: PathBuf,
        heading: String,
    },
    PackageManifestParseFailed {
        path: PathBuf,
        error: String,
    },
    InvalidPackageManifestField {
        path: PathBuf,
        field: String,
        expected: String,
    },
    MissingPackageScript {
        path: PathBuf,
        script: String,
    },
    UndeclaredPackageImport {
        path: PathBuf,
        specifier: String,
        package_name: String,
    },
    UnusedPackageDependency {
        path: PathBuf,
        package_name: String,
    },
    InvalidWorkflowDependencyMetadata {
        path: PathBuf,
        field: String,
    },
    WorkflowDependencyMetadataMismatch {
        path: PathBuf,
        package_name: String,
        source: String,
        target: String,
    },
    MissingValidationCommands {
        path: PathBuf,
    },
    EmptyValidationCommands {
        path: PathBuf,
    },
    InvalidValidationCommands {
        path: PathBuf,
    },
    MissingBuildValidationCommand {
        path: PathBuf,
    },
    MissingTestValidationCommand {
        path: PathBuf,
    },
    MissingContractSmoke {
        path: PathBuf,
    },
    InvalidContractSmoke {
        path: PathBuf,
    },
    MissingCoverageMetadata {
        path: PathBuf,
    },
    MissingCoverageKey {
        path: PathBuf,
        key: String,
    },
    InvalidCoverageKeyType {
        path: PathBuf,
        key: String,
    },
    CoverageKeyMustBeTrue {
        path: PathBuf,
        key: String,
    },
    MissingCoverageMarker {
        path: PathBuf,
        key: String,
    },
    CodeOutsideSrc {
        paths: Vec<PathBuf>,
    },
    TestsOutsideSrcTests {
        paths: Vec<PathBuf>,
    },
    DatabasesOutsideState {
        paths: Vec<PathBuf>,
    },
    RuntimeStateGitignoreMissing {
        path: PathBuf,
        patterns: Vec<String>,
    },
    TrackedRuntimeStateFiles {
        paths: Vec<PathBuf>,
    },
    AmbiguousWorkflowOutputSchema {
        path: PathBuf,
        schema_path: String,
    },
    ValidationCommandFailed {
        command: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    WorkflowApiContractExtractionFailed {
        path: PathBuf,
        error: String,
    },
    WorkflowApiContractSmokeFailed {
        command: String,
        error: String,
        stdout: String,
        stderr: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowValidationCommandResult {
    pub command: String,
    pub succeeded: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowValidationInfo {
    pub status: WorkflowValidationStatus,
    pub findings: Vec<WorkflowValidationFindingInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRootInfo {
    pub kind: WorkflowRootKind,
    pub label: String,
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCommandOptionHint {
    pub display: String,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowSummary {
    pub id: String,
    pub command: Option<String>,
    pub title: Option<String>,
    pub user_description: Option<String>,
    pub search_terms: Vec<String>,
    pub command_option_hints: Vec<WorkflowCommandOptionHint>,
    pub root_label: String,
    pub root_kind: WorkflowRootKind,
    pub root_path: PathBuf,
    pub path: PathBuf,
    pub workflow_yaml_path: PathBuf,
    pub mention_target: String,
    pub validation: WorkflowValidationInfo,
    pub repair_mode: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactInfo {
    pub id: String,
    pub path: PathBuf,
    pub dependencies: Vec<String>,
    pub dev_dependencies: Vec<String>,
    pub git_status: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case", export_to = "v2/")]
pub struct WorkflowConfigValues {
    pub search_paths: Vec<PathBuf>,
    pub default_location: String,
    pub repair_mode: String,
    pub max_repair_cycles: u32,
    pub dependency_update_policy: String,
    pub commit_policy: String,
    pub validation_profile: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListParams {
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowListResponse {
    pub roots: Vec<WorkflowRootInfo>,
    pub workflows: Vec<WorkflowSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowReadParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub target: Option<String>,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowReadResponse {
    pub workflow: WorkflowSummary,
    pub workflow_yaml: String,
    pub readme: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowImpactResponse {
    pub impact: WorkflowImpactInfo,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowDevelopParams {
    pub description: String,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowEditParams {
    pub id: String,
    pub instruction: String,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRunApprovalHandling {
    Delegate,
    Decline,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRunStatus {
    Running,
    Succeeded,
    Failed,
    Canceled,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub status: WorkflowRunStatus,
    pub thread_id: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub output: Option<JsonValue>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunStartParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub input: Option<JsonValue>,
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
    #[ts(optional = nullable)]
    pub approval_handling: Option<WorkflowRunApprovalHandling>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunStartResponse {
    pub run: WorkflowRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunReadParams {
    pub run_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunReadResponse {
    pub run: WorkflowRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunWaitParams {
    pub run_id: String,
    #[ts(optional = nullable)]
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunWaitResponse {
    pub run: WorkflowRun,
    pub completed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunCancelParams {
    pub run_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunCancelResponse {
    pub run: WorkflowRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowValidateParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRepairParams {
    pub id: String,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStageSessionActionParams {
    pub stage_session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCommandResponse {
    pub message: String,
    pub data: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowThreadStatus {
    pub name: String,
    pub status: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowChildStatus {
    pub workflow_name: String,
    pub workflow_status: String,
    pub threads: Vec<WorkflowThreadStatus>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowStatusUpdate {
    pub workflow_name: String,
    pub workflow_status: String,
    pub threads: Vec<WorkflowThreadStatus>,
    pub child_statuses: Vec<WorkflowChildStatus>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowProgressNotification {
    pub run_id: String,
    pub thread_id: Option<String>,
    pub message: String,
    pub data: Option<JsonValue>,
    pub status: Option<WorkflowStatusUpdate>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowMarkdownResultNotification {
    pub run_id: String,
    pub thread_id: Option<String>,
    pub markdown: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunCompletedNotification {
    pub run: WorkflowRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRunFailedNotification {
    pub run: WorkflowRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRepairAction {
    pub kind: WorkflowRepairActionKind,
    pub path: PathBuf,
    pub detail: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum WorkflowRepairStopReason {
    Valid,
    BlockedByRepairMode,
    UnsupportedFindings,
    RepairBudgetExhausted,
    NoChangesApplied,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
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

macro_rules! workflow_command_response_type {
    ($name:ident) => {
        #[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        #[ts(export_to = "v2/")]
        pub struct $name {
            pub message: String,
            pub data: JsonValue,
        }

        impl From<WorkflowCommandResponse> for $name {
            fn from(response: WorkflowCommandResponse) -> Self {
                Self {
                    message: response.message,
                    data: response.data,
                }
            }
        }
    };
}

workflow_command_response_type!(WorkflowDevelopResponse);
workflow_command_response_type!(WorkflowEditResponse);
workflow_command_response_type!(WorkflowPublishResponse);
workflow_command_response_type!(WorkflowDiscardResponse);
workflow_command_response_type!(WorkflowValidateResponse);
workflow_command_response_type!(WorkflowCommandExecuteResponse);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowRepairResponse {
    pub message: String,
    pub workflow: WorkflowSummary,
    pub validation: WorkflowValidationInfo,
    pub validation_command_results: Vec<WorkflowValidationCommandResult>,
    pub repair: WorkflowRepairResult,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigReadResponse {
    pub config: WorkflowConfigValues,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigWriteParams {
    pub key: String,
    #[ts(optional = nullable)]
    pub value: Option<JsonValue>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowConfigWriteResponse {
    pub config: WorkflowConfigValues,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowCommandExecuteParams {
    pub args: Vec<String>,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthoringContextPrepareParams {
    #[ts(optional = nullable)]
    pub id: Option<String>,
    #[ts(optional = nullable)]
    pub description: Option<String>,
    #[ts(optional = nullable)]
    pub stage_session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkflowAuthoringContextPrepareResponse {
    pub roots: Vec<WorkflowRootInfo>,
    pub workflows: Vec<WorkflowSummary>,
    pub config: WorkflowConfigValues,
}
