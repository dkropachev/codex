use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowValidationFinding {
    WorkflowSpecReadFailed { path: PathBuf, error: String },
    WorkflowIdMismatch {
        path: PathBuf,
        expected_id: String,
        actual_id: String,
    },
    MissingFile { path: PathBuf },
    MissingDirectory { path: PathBuf },
    MissingGitRepository { path: PathBuf },
    WorkflowPathEscapesRoot { workflow_path: PathBuf, root_path: PathBuf },
    MissingDocumentHeading { path: PathBuf, heading: String },
    PackageManifestParseFailed { path: PathBuf, error: String },
    UndeclaredPackageImport {
        path: PathBuf,
        specifier: String,
        package_name: String,
    },
    MissingValidationCommands { path: PathBuf },
    EmptyValidationCommands { path: PathBuf },
    InvalidValidationCommands { path: PathBuf },
    MissingCoverageMetadata { path: PathBuf },
    MissingCoverageKey { path: PathBuf, key: String },
    InvalidCoverageKeyType { path: PathBuf, key: String },
    CoverageKeyMustBeTrue { path: PathBuf, key: String },
    MissingCoverageMarker { path: PathBuf, key: String },
    CodeOutsideSrc { paths: Vec<PathBuf> },
    TestsOutsideSrcTests { paths: Vec<PathBuf> },
    DatabasesOutsideState { paths: Vec<PathBuf> },
    ValidationCommandFailed {
        command: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    WorkflowApiContractExtractionFailed { path: PathBuf, error: String },
}

impl WorkflowValidationFinding {
    pub fn message(&self) -> String {
        match self {
            Self::WorkflowSpecReadFailed { path, error } => {
                format!("failed to read workflow metadata {}: {error}", path.display())
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
            Self::PackageManifestParseFailed { path, error } => {
                format!("failed to parse package manifest {}: {error}", path.display())
            }
            Self::UndeclaredPackageImport { package_name, .. } => {
                format!("imports undeclared package `{package_name}`")
            }
            Self::MissingValidationCommands { .. } => "missing validation commands".to_string(),
            Self::EmptyValidationCommands { .. } => "validation commands are empty".to_string(),
            Self::InvalidValidationCommands { .. } => {
                "validation commands must be a non-empty string array".to_string()
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
                format!("workflow source exists outside src/: {}", display_paths(paths))
            }
            Self::TestsOutsideSrcTests { paths } => {
                format!("test files must live under src/tests/: {}", display_paths(paths))
            }
            Self::DatabasesOutsideState { paths } => {
                format!("database files must live under state/: {}", display_paths(paths))
            }
            Self::ValidationCommandFailed {
                command,
                exit_code,
                ..
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
            Self::PackageManifestParseFailed { .. } | Self::UndeclaredPackageImport { .. } => {
                "WF-004"
            }
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
            | Self::ValidationCommandFailed { .. }
            | Self::WorkflowApiContractExtractionFailed { .. } => "WF-007",
            Self::MissingDirectory { .. }
            | Self::MissingGitRepository { .. }
            | Self::WorkflowPathEscapesRoot { .. }
            | Self::CodeOutsideSrc { .. }
            | Self::TestsOutsideSrcTests { .. }
            | Self::DatabasesOutsideState { .. } => "WF-003",
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
            | Self::PackageManifestParseFailed { path, .. }
            | Self::UndeclaredPackageImport { path, .. }
            | Self::MissingValidationCommands { path }
            | Self::EmptyValidationCommands { path }
            | Self::InvalidValidationCommands { path }
            | Self::MissingCoverageMetadata { path }
            | Self::MissingCoverageKey { path, .. }
            | Self::InvalidCoverageKeyType { path, .. }
            | Self::CoverageKeyMustBeTrue { path, .. }
            | Self::MissingCoverageMarker { path, .. }
            | Self::WorkflowApiContractExtractionFailed { path, .. } => path.clone(),
            Self::WorkflowPathEscapesRoot { workflow_path, .. } => workflow_path.clone(),
            Self::CodeOutsideSrc { paths }
            | Self::TestsOutsideSrcTests { paths }
            | Self::DatabasesOutsideState { paths } => paths
                .first()
                .cloned()
                .unwrap_or_else(|| workflow_path.to_path_buf()),
            Self::ValidationCommandFailed { .. } => workflow_path.to_path_buf(),
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
    paths.iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
