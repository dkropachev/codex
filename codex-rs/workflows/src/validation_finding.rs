use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkflowValidationFinding {
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
    NonBunPackageScript {
        path: PathBuf,
        script: String,
        command: String,
    },
    DisallowedPackageDependency {
        path: PathBuf,
        package_name: String,
    },
    UndeclaredPackageImport {
        path: PathBuf,
        specifier: String,
        package_name: String,
    },
    DisallowedNodeRuntimeImport {
        path: PathBuf,
        specifier: String,
    },
    DisallowedWorkflowRuntimeFile {
        path: PathBuf,
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
    NonBunValidationCommand {
        path: PathBuf,
        command: String,
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

impl WorkflowValidationFinding {
    pub fn message(&self) -> String {
        match self {
            Self::WorkflowSpecReadFailed { path, error } => {
                format!(
                    "failed to read workflow metadata {}: {error}",
                    path.display()
                )
            }
            Self::WorkflowIdMismatch {
                expected_id,
                actual_id,
                ..
            } => format!(
                "workflow.yaml id '{actual_id}' does not match directory id '{expected_id}'"
            ),
            Self::MissingFile { path } => format!(
                "missing {}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| path.display().to_string())
            ),
            Self::MissingDirectory { path } => {
                format!("missing directory {}", path.display())
            }
            Self::MissingGitRepository { path } => {
                format!("missing git repository at {}", path.display())
            }
            Self::WorkflowPathEscapesRoot {
                workflow_path,
                root_path,
            } => format!(
                "workflow path {} escapes root {}",
                workflow_path.display(),
                root_path.display()
            ),
            Self::MissingDocumentHeading { path, heading } => {
                format!("{} is missing required heading `{heading}`", path.display())
            }
            Self::EmptyDocumentSection { path, heading } => {
                format!(
                    "{} section `{heading}` must describe the workflow",
                    path.display()
                )
            }
            Self::PackageManifestParseFailed { path, error } => {
                format!(
                    "failed to parse package manifest {}: {error}",
                    path.display()
                )
            }
            Self::InvalidPackageManifestField {
                field, expected, ..
            } => format!("package.json field `{field}` must be {expected}"),
            Self::MissingPackageScript { script, .. } => {
                format!("package.json is missing required `{script}` script")
            }
            Self::NonBunPackageScript {
                script, command, ..
            } => {
                format!("package.json `{script}` script must use Bun, found `{command}`")
            }
            Self::DisallowedPackageDependency { package_name, .. } => {
                format!("package.json dependency `{package_name}` is not allowed in Bun workflows")
            }
            Self::UndeclaredPackageImport { package_name, .. } => {
                format!("imports undeclared package `{package_name}`")
            }
            Self::DisallowedNodeRuntimeImport { specifier, .. } => {
                format!("imports Node-only runtime module `{specifier}`")
            }
            Self::DisallowedWorkflowRuntimeFile { path } => {
                format!("{} is not allowed for Bun workflows", path.display())
            }
            Self::UnusedPackageDependency { package_name, .. } => {
                format!("package.json declares unused runtime dependency `{package_name}`")
            }
            Self::InvalidWorkflowDependencyMetadata { field, .. } => {
                format!("workflow.yaml field `{field}` must be an array of package names")
            }
            Self::WorkflowDependencyMetadataMismatch {
                package_name,
                source,
                target,
                ..
            } => {
                format!("package `{package_name}` is listed in {source} but missing from {target}")
            }
            Self::MissingValidationCommands { .. } => "missing validation commands".to_string(),
            Self::EmptyValidationCommands { .. } => "validation commands are empty".to_string(),
            Self::InvalidValidationCommands { .. } => {
                "validation commands must be a non-empty string array".to_string()
            }
            Self::MissingBuildValidationCommand { .. } => {
                "validation commands must include a build/typecheck step".to_string()
            }
            Self::MissingTestValidationCommand { .. } => {
                "validation commands must include a test step".to_string()
            }
            Self::NonBunValidationCommand { command, .. } => {
                format!("validation command `{command}` must use Bun")
            }
            Self::MissingContractSmoke { .. } => {
                "validation.contractSmoke must be configured".to_string()
            }
            Self::InvalidContractSmoke { .. } => {
                "validation.contractSmoke must be true, a command string, or an enabled object"
                    .to_string()
            }
            Self::MissingCoverageMetadata { .. } => {
                "missing validation coverage metadata".to_string()
            }
            Self::MissingCoverageKey { key, .. } => {
                format!("missing coverage key `{key}`")
            }
            Self::InvalidCoverageKeyType { key, .. } => {
                format!("coverage key `{key}` must be a boolean")
            }
            Self::CoverageKeyMustBeTrue { key, .. } => {
                format!("coverage key `{key}` must be true")
            }
            Self::MissingCoverageMarker { key, .. } => {
                format!("missing test coverage marker `// workflow-covers: {key}`")
            }
            Self::CodeOutsideSrc { paths } => {
                format!(
                    "workflow source exists outside src/: {}",
                    display_paths(paths)
                )
            }
            Self::TestsOutsideSrcTests { paths } => {
                format!(
                    "test files must live under src/tests/: {}",
                    display_paths(paths)
                )
            }
            Self::DatabasesOutsideState { paths } => {
                format!(
                    "database files must live under state/: {}",
                    display_paths(paths)
                )
            }
            Self::RuntimeStateGitignoreMissing { patterns, .. } => format!(
                "runtime state ignore rules are missing from .gitignore: {}",
                patterns.join(", ")
            ),
            Self::TrackedRuntimeStateFiles { paths } => {
                format!(
                    "runtime state files must not be tracked by git: {}",
                    display_paths(paths)
                )
            }
            Self::AmbiguousWorkflowOutputSchema { schema_path, .. } => format!(
                "workflow output schema at {schema_path} must declare properties or additionalProperties explicitly"
            ),
            Self::ValidationCommandFailed {
                command, exit_code, ..
            } => match exit_code {
                Some(exit_code) => {
                    format!("validation command `{command}` failed with exit code {exit_code}")
                }
                None => format!("validation command `{command}` failed"),
            },
            Self::WorkflowApiContractExtractionFailed { path, error } => format!(
                "failed to extract workflow API contract from {}: {error}",
                path.display()
            ),
            Self::WorkflowApiContractSmokeFailed { command, error, .. } => {
                format!("workflow contract smoke command `{command}` failed: {error}")
            }
        }
    }

    pub fn rule_id(&self) -> &'static str {
        match self {
            Self::MissingFile { path } | Self::MissingDocumentHeading { path, .. }
                if path.file_name().and_then(|name| name.to_str()) == Some("README.md") =>
            {
                "WF-001"
            }
            Self::MissingFile { path } | Self::MissingDocumentHeading { path, .. }
                if path.file_name().and_then(|name| name.to_str()) == Some("DESIGN.md") =>
            {
                "WF-002"
            }
            Self::MissingFile { .. } | Self::MissingDocumentHeading { .. } => "WF-011",
            Self::EmptyDocumentSection { path, .. }
                if path.file_name().and_then(|name| name.to_str()) == Some("README.md") =>
            {
                "WF-001"
            }
            Self::EmptyDocumentSection { path, .. }
                if path.file_name().and_then(|name| name.to_str()) == Some("DESIGN.md") =>
            {
                "WF-002"
            }
            Self::EmptyDocumentSection { .. } => "WF-011",
            Self::PackageManifestParseFailed { .. }
            | Self::InvalidPackageManifestField { .. }
            | Self::MissingPackageScript { .. }
            | Self::NonBunPackageScript { .. }
            | Self::DisallowedPackageDependency { .. }
            | Self::UndeclaredPackageImport { .. }
            | Self::DisallowedNodeRuntimeImport { .. }
            | Self::DisallowedWorkflowRuntimeFile { .. }
            | Self::UnusedPackageDependency { .. }
            | Self::InvalidWorkflowDependencyMetadata { .. }
            | Self::WorkflowDependencyMetadataMismatch { .. } => "WF-004",
            Self::MissingCoverageMetadata { .. } => "WF-008",
            Self::MissingCoverageKey { key, .. }
            | Self::InvalidCoverageKeyType { key, .. }
            | Self::CoverageKeyMustBeTrue { key, .. }
            | Self::MissingCoverageMarker { key, .. } => match key.as_str() {
                "negative" | "failureUx" => "WF-009",
                "recovery" => "WF-010",
                _ => "WF-008",
            },
            Self::MissingValidationCommands { .. }
            | Self::EmptyValidationCommands { .. }
            | Self::InvalidValidationCommands { .. }
            | Self::MissingBuildValidationCommand { .. }
            | Self::MissingTestValidationCommand { .. }
            | Self::NonBunValidationCommand { .. }
            | Self::MissingContractSmoke { .. }
            | Self::InvalidContractSmoke { .. }
            | Self::ValidationCommandFailed { .. }
            | Self::AmbiguousWorkflowOutputSchema { .. }
            | Self::WorkflowApiContractExtractionFailed { .. }
            | Self::WorkflowApiContractSmokeFailed { .. } => "WF-007",
            Self::MissingDirectory { .. }
            | Self::MissingGitRepository { .. }
            | Self::WorkflowPathEscapesRoot { .. }
            | Self::CodeOutsideSrc { .. }
            | Self::TestsOutsideSrcTests { .. }
            | Self::DatabasesOutsideState { .. }
            | Self::RuntimeStateGitignoreMissing { .. }
            | Self::TrackedRuntimeStateFiles { .. } => "WF-003",
            Self::WorkflowIdMismatch { .. } => "WF-007",
            Self::WorkflowSpecReadFailed { .. } => "WF-011",
        }
    }

    pub fn title(&self) -> &'static str {
        match self.rule_id() {
            "WF-001" => "README.md is incomplete or missing",
            "WF-002" => "DESIGN.md is incomplete or missing",
            "WF-003" => "Workflow layout is invalid",
            "WF-004" => "Workflow dependencies are not self-contained",
            "WF-007" => "Workflow validation metadata or commands are inaccurate",
            "WF-008" => "Positive-path coverage is missing or inaccurate",
            "WF-009" => "Negative and failure-path coverage is missing or inaccurate",
            "WF-010" => "Recovery coverage is missing or inaccurate",
            _ => "Workflow validation surfaced a stability or correctness issue",
        }
    }

    pub fn resolved_primary_path(&self, workflow_path: &Path) -> PathBuf {
        match self {
            Self::WorkflowSpecReadFailed { path, .. }
            | Self::WorkflowIdMismatch { path, .. }
            | Self::MissingFile { path }
            | Self::MissingDirectory { path }
            | Self::MissingGitRepository { path }
            | Self::MissingDocumentHeading { path, .. }
            | Self::EmptyDocumentSection { path, .. }
            | Self::PackageManifestParseFailed { path, .. }
            | Self::InvalidPackageManifestField { path, .. }
            | Self::MissingPackageScript { path, .. }
            | Self::NonBunPackageScript { path, .. }
            | Self::DisallowedPackageDependency { path, .. }
            | Self::UndeclaredPackageImport { path, .. }
            | Self::DisallowedNodeRuntimeImport { path, .. }
            | Self::DisallowedWorkflowRuntimeFile { path }
            | Self::UnusedPackageDependency { path, .. }
            | Self::InvalidWorkflowDependencyMetadata { path, .. }
            | Self::WorkflowDependencyMetadataMismatch { path, .. }
            | Self::MissingValidationCommands { path }
            | Self::EmptyValidationCommands { path }
            | Self::InvalidValidationCommands { path }
            | Self::MissingBuildValidationCommand { path }
            | Self::MissingTestValidationCommand { path }
            | Self::NonBunValidationCommand { path, .. }
            | Self::MissingContractSmoke { path }
            | Self::InvalidContractSmoke { path }
            | Self::MissingCoverageMetadata { path }
            | Self::MissingCoverageKey { path, .. }
            | Self::InvalidCoverageKeyType { path, .. }
            | Self::CoverageKeyMustBeTrue { path, .. }
            | Self::MissingCoverageMarker { path, .. }
            | Self::RuntimeStateGitignoreMissing { path, .. }
            | Self::AmbiguousWorkflowOutputSchema { path, .. }
            | Self::WorkflowApiContractExtractionFailed { path, .. } => path.clone(),
            Self::WorkflowPathEscapesRoot { workflow_path, .. } => workflow_path.clone(),
            Self::CodeOutsideSrc { paths }
            | Self::TestsOutsideSrcTests { paths }
            | Self::DatabasesOutsideState { paths }
            | Self::TrackedRuntimeStateFiles { paths } => paths
                .first()
                .cloned()
                .unwrap_or_else(|| workflow_path.to_path_buf()),
            Self::ValidationCommandFailed { .. } | Self::WorkflowApiContractSmokeFailed { .. } => {
                workflow_path.to_path_buf()
            }
        }
    }
}

pub fn finding_messages(findings: &[WorkflowValidationFinding]) -> Vec<String> {
    findings
        .iter()
        .map(WorkflowValidationFinding::message)
        .collect()
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
