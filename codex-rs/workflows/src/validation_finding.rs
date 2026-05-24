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
    PackageManifestParseFailed {
        path: PathBuf,
        error: String,
    },
    UndeclaredPackageImport {
        path: PathBuf,
        specifier: String,
        package_name: String,
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
}

impl WorkflowValidationFinding {
    pub fn message(&self) -> String {
        match self {
            Self::WorkflowSpecReadFailed { path, error } => {
                format!("failed to read workflow spec {}: {error}", path.display())
            }
            Self::WorkflowIdMismatch {
                expected_id,
                actual_id,
                ..
            } => format!(
                "workflow.yaml id '{actual_id}' does not match directory id '{expected_id}'"
            ),
            Self::MissingFile { path } => format!("missing {}", path.display()),
            Self::MissingDirectory { path } => format!("missing directory {}", path.display()),
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
                format!(
                    "missing document heading `## {heading}` in {}",
                    path.display()
                )
            }
            Self::PackageManifestParseFailed { path, error } => {
                format!(
                    "failed to parse package manifest {}: {error}",
                    path.display()
                )
            }
            Self::UndeclaredPackageImport {
                path, package_name, ..
            } => format!(
                "{} imports undeclared package `{package_name}`",
                path.display()
            ),
            Self::MissingValidationCommands { path } => {
                format!("{} is missing validation commands", path.display())
            }
            Self::EmptyValidationCommands { path } => {
                format!("{} validation.commands must not be empty", path.display())
            }
            Self::InvalidValidationCommands { path } => {
                format!(
                    "{} validation.commands must be an array of strings",
                    path.display()
                )
            }
            Self::MissingCoverageMetadata { path } => {
                format!("{} is missing validation.coverage metadata", path.display())
            }
            Self::MissingCoverageKey { path, key } => {
                format!("{} coverage is missing key `{key}`", path.display())
            }
            Self::InvalidCoverageKeyType { path, key } => {
                format!("{} coverage key `{key}` must be a boolean", path.display())
            }
            Self::CoverageKeyMustBeTrue { path, key } => {
                format!("{} coverage key `{key}` must be true", path.display())
            }
            Self::MissingCoverageMarker { path, key } => format!(
                "missing test coverage marker `// workflow-covers: {key}` in {}",
                path.display()
            ),
            Self::CodeOutsideSrc { paths } => {
                format!("code files must live under src/: {}", join_paths(paths))
            }
            Self::TestsOutsideSrcTests { paths } => {
                format!(
                    "test files must live under src/tests/: {}",
                    join_paths(paths)
                )
            }
            Self::DatabasesOutsideState { paths } => {
                format!(
                    "database files must live under state/: {}",
                    join_paths(paths)
                )
            }
            Self::ValidationCommandFailed {
                command, exit_code, ..
            } => match exit_code {
                Some(code) => {
                    format!("validation command `{command}` failed with exit code {code}")
                }
                None => format!("validation command `{command}` failed"),
            },
            Self::WorkflowApiContractExtractionFailed { path, error } => format!(
                "failed to extract workflow API contract from {}: {error}",
                path.display()
            ),
        }
    }

    pub(crate) fn rule_id(&self) -> &'static str {
        match self {
            Self::WorkflowSpecReadFailed { .. } => "WF-001",
            Self::MissingFile { .. } => "WF-002",
            Self::MissingDirectory { .. } => "WF-003",
            Self::MissingGitRepository { .. } => "WF-004",
            Self::WorkflowPathEscapesRoot { .. } => "WF-005",
            Self::MissingDocumentHeading { .. } => "WF-006",
            Self::WorkflowIdMismatch { .. } => "WF-007",
            Self::PackageManifestParseFailed { .. } => "WF-008",
            Self::UndeclaredPackageImport { .. } => "WF-009",
            Self::MissingValidationCommands { .. } => "WF-010",
            Self::EmptyValidationCommands { .. } => "WF-010",
            Self::InvalidValidationCommands { .. } => "WF-010",
            Self::ValidationCommandFailed { .. } => "WF-011",
            Self::MissingCoverageMetadata { .. } => "WF-012",
            Self::MissingCoverageKey { .. } => "WF-013",
            Self::InvalidCoverageKeyType { .. } => "WF-013",
            Self::CoverageKeyMustBeTrue { .. } => "WF-013",
            Self::MissingCoverageMarker { .. } => "WF-014",
            Self::CodeOutsideSrc { .. } => "WF-016",
            Self::TestsOutsideSrcTests { .. } => "WF-017",
            Self::DatabasesOutsideState { .. } => "WF-018",
            Self::WorkflowApiContractExtractionFailed { .. } => "WF-019",
        }
    }

    pub(crate) fn title(&self) -> &'static str {
        match self {
            Self::WorkflowSpecReadFailed { .. } => "Workflow spec read failed",
            Self::WorkflowIdMismatch { .. } => "Workflow ID mismatch",
            Self::MissingFile { .. } => "Missing file",
            Self::MissingDirectory { .. } => "Missing directory",
            Self::MissingGitRepository { .. } => "Missing git repository",
            Self::WorkflowPathEscapesRoot { .. } => "Workflow path escapes root",
            Self::MissingDocumentHeading { .. } => "Missing document heading",
            Self::PackageManifestParseFailed { .. } => "Package manifest parse failed",
            Self::UndeclaredPackageImport { .. } => "Undeclared package import",
            Self::MissingValidationCommands { .. } => "Missing validation commands",
            Self::EmptyValidationCommands { .. } => "Empty validation commands",
            Self::InvalidValidationCommands { .. } => "Invalid validation commands",
            Self::MissingCoverageMetadata { .. } => "Missing coverage metadata",
            Self::MissingCoverageKey { .. } => "Missing coverage key",
            Self::InvalidCoverageKeyType { .. } => "Invalid coverage key type",
            Self::CoverageKeyMustBeTrue { .. } => "Coverage key must be true",
            Self::MissingCoverageMarker { .. } => "Missing coverage marker",
            Self::CodeOutsideSrc { .. } => "Code outside src",
            Self::TestsOutsideSrcTests { .. } => "Tests outside src/tests",
            Self::DatabasesOutsideState { .. } => "Databases outside state",
            Self::ValidationCommandFailed { .. } => "Validation command failed",
            Self::WorkflowApiContractExtractionFailed { .. } => {
                "Workflow API contract extraction failed"
            }
        }
    }

    pub(crate) fn resolved_primary_path(&self, workflow_path: &Path) -> PathBuf {
        match self {
            Self::WorkflowSpecReadFailed { path, .. }
            | Self::MissingFile { path }
            | Self::MissingDirectory { path }
            | Self::MissingGitRepository { path }
            | Self::MissingDocumentHeading { path, .. }
            | Self::PackageManifestParseFailed { path, .. }
            | Self::MissingValidationCommands { path }
            | Self::EmptyValidationCommands { path }
            | Self::InvalidValidationCommands { path }
            | Self::MissingCoverageMetadata { path }
            | Self::MissingCoverageKey { path, .. }
            | Self::InvalidCoverageKeyType { path, .. }
            | Self::CoverageKeyMustBeTrue { path, .. }
            | Self::MissingCoverageMarker { path, .. }
            | Self::WorkflowApiContractExtractionFailed { path, .. } => {
                resolve_path(workflow_path, path)
            }
            Self::WorkflowPathEscapesRoot { workflow_path, .. } => workflow_path.clone(),
            Self::WorkflowIdMismatch { path, .. } => resolve_path(workflow_path, path),
            Self::UndeclaredPackageImport { path, .. } => resolve_path(workflow_path, path),
            Self::CodeOutsideSrc { paths }
            | Self::TestsOutsideSrcTests { paths }
            | Self::DatabasesOutsideState { paths } => paths.first().map_or_else(
                || workflow_path.to_path_buf(),
                |path| resolve_path(workflow_path, path),
            ),
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

fn resolve_path(workflow_path: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workflow_path.join(path)
    }
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
