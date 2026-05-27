use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value as JsonValue;

use crate::registry::WorkflowValidation;
use crate::registry::WorkflowValidationStatus;
use crate::spec::WORKFLOW_YAML;
use crate::spec::read_workflow_spec;
use crate::validation_finding::WorkflowValidationFinding;
use crate::validation_package::validate_local_package_imports;
use crate::validation_package::validate_package_manifest;

const REQUIRED_FILES: &[&str] = &[
    "README.md",
    "DESIGN.md",
    "package.json",
    "tsconfig.json",
    "src/workflow.ts",
];
const REQUIRED_DIRS: &[&str] = &["src", "src/tests", "state"];
const REQUIRED_README_HEADINGS: &[&str] = &[
    "Usage",
    "Workflow Runtime",
    "Dependencies",
    "Validation",
    "Maintenance",
];
const REQUIRED_DESIGN_HEADINGS: &[&str] = &[
    "Overview",
    "Architecture",
    "Data Flow",
    "Failure Handling",
    "Recovery Behavior",
    "Test Matrix",
    "Maintenance Notes",
];
const REQUIRED_COVERAGE_KEYS: &[&str] = &[
    "positive",
    "negative",
    "progress",
    "finalResult",
    "failureUx",
    "load",
    "autocomplete",
    "recovery",
];
const REQUIRED_MARKERS: &[&str] = &[
    "positive",
    "negative",
    "progress",
    "finalResult",
    "failureUx",
    "load",
    "autocomplete",
];
const REQUIRED_TRUE_COVERAGE_KEYS: &[&str] = &[
    "positive",
    "negative",
    "progress",
    "finalResult",
    "failureUx",
    "load",
    "autocomplete",
];
const WORKFLOW_TEST_MARKER_PREFIX: &str = "workflow-covers:";
const RUNTIME_STATE_GITIGNORE_PATTERNS: &[&str] =
    &["node_modules/", "artifacts/", "state/*", "!state/.gitkeep"];

pub(crate) fn validate_workflow_dir(
    root: &Path,
    workflow_dir: &Path,
    expected_id: &str,
) -> WorkflowValidation {
    let mut findings = Vec::new();

    let spec_path = workflow_dir.join(WORKFLOW_YAML);
    let spec = match read_workflow_spec(&spec_path) {
        Ok(spec) => {
            if spec.id != expected_id {
                findings.push(WorkflowValidationFinding::WorkflowIdMismatch {
                    path: PathBuf::from(WORKFLOW_YAML),
                    expected_id: expected_id.to_string(),
                    actual_id: spec.id.clone(),
                });
            }
            Some(spec)
        }
        Err(err) => {
            findings.push(WorkflowValidationFinding::WorkflowSpecReadFailed {
                path: spec_path,
                error: err.to_string(),
            });
            None
        }
    };

    for relative in REQUIRED_FILES {
        if !workflow_dir.join(relative).is_file() {
            findings.push(WorkflowValidationFinding::MissingFile {
                path: PathBuf::from(relative),
            });
        }
    }
    for relative in REQUIRED_DIRS {
        if !workflow_dir.join(relative).is_dir() {
            findings.push(WorkflowValidationFinding::MissingDirectory {
                path: PathBuf::from(relative),
            });
        }
    }
    if !workflow_dir.join(".git").is_dir() {
        findings.push(WorkflowValidationFinding::MissingGitRepository {
            path: PathBuf::from(".git"),
        });
    }
    if !workflow_dir.starts_with(root) {
        findings.push(WorkflowValidationFinding::WorkflowPathEscapesRoot {
            workflow_path: workflow_dir.to_path_buf(),
            root_path: root.to_path_buf(),
        });
    }

    findings.extend(validate_document_headings(
        workflow_dir,
        "README.md",
        REQUIRED_README_HEADINGS,
    ));
    findings.extend(validate_document_headings(
        workflow_dir,
        "DESIGN.md",
        REQUIRED_DESIGN_HEADINGS,
    ));
    findings.extend(validate_package_manifest(workflow_dir, spec.as_ref()));
    findings.extend(validate_local_package_imports(workflow_dir));
    if let Some(spec) = spec.as_ref() {
        findings.extend(validate_validation_commands(spec));
        findings.extend(validate_coverage_metadata(workflow_dir, spec));
        findings.extend(validate_contract_smoke_metadata(spec));
        findings.extend(validate_output_schemas(spec));
    }
    findings.extend(validate_runtime_state_gitignore(workflow_dir));
    findings.extend(validate_tracked_runtime_state(workflow_dir));

    let mut code_outside_src = Vec::new();
    let mut tests_outside_src_tests = Vec::new();
    let mut databases_outside_state = Vec::new();
    collect_layout_issues(
        workflow_dir,
        workflow_dir,
        &mut code_outside_src,
        &mut tests_outside_src_tests,
        &mut databases_outside_state,
    );
    if !code_outside_src.is_empty() {
        findings.push(WorkflowValidationFinding::CodeOutsideSrc {
            paths: code_outside_src,
        });
    }
    if !tests_outside_src_tests.is_empty() {
        findings.push(WorkflowValidationFinding::TestsOutsideSrcTests {
            paths: tests_outside_src_tests,
        });
    }
    if !databases_outside_state.is_empty() {
        findings.push(WorkflowValidationFinding::DatabasesOutsideState {
            paths: databases_outside_state,
        });
    }

    let status = if findings.is_empty() {
        WorkflowValidationStatus::Valid
    } else {
        WorkflowValidationStatus::Invalid
    };
    WorkflowValidation { status, findings }
}

fn validate_document_headings(
    workflow_dir: &Path,
    file_name: &str,
    required_headings: &[&str],
) -> Vec<WorkflowValidationFinding> {
    let path = workflow_dir.join(file_name);
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };

    let sections = markdown_sections(&contents);
    let mut findings = Vec::new();
    for heading in required_headings {
        if let Some(body) = sections.get(*heading) {
            if body.trim().is_empty() {
                findings.push(WorkflowValidationFinding::EmptyDocumentSection {
                    path: PathBuf::from(file_name),
                    heading: (*heading).to_string(),
                });
            }
        } else {
            findings.push(WorkflowValidationFinding::MissingDocumentHeading {
                path: PathBuf::from(file_name),
                heading: (*heading).to_string(),
            });
        }
    }
    findings
}

fn markdown_sections(contents: &str) -> BTreeMap<String, String> {
    let mut sections = BTreeMap::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = String::new();
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if let Some(heading) = current_heading.replace(heading.trim().to_string()) {
                sections.entry(heading).or_insert(current_body);
                current_body = String::new();
            }
            continue;
        }
        if current_heading.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if let Some(heading) = current_heading {
        sections.entry(heading).or_insert(current_body);
    }
    sections
}

fn validate_coverage_metadata(
    workflow_dir: &Path,
    spec: &crate::spec::WorkflowSpec,
) -> Vec<WorkflowValidationFinding> {
    let Some(coverage) = spec
        .validation
        .get("coverage")
        .and_then(JsonValue::as_object)
    else {
        return vec![WorkflowValidationFinding::MissingCoverageMetadata {
            path: PathBuf::from(WORKFLOW_YAML),
        }];
    };

    let mut findings = Vec::new();
    for key in REQUIRED_COVERAGE_KEYS {
        let require_true = REQUIRED_TRUE_COVERAGE_KEYS.contains(key);
        match coverage.get(*key) {
            Some(JsonValue::Bool(true)) => {}
            Some(JsonValue::Bool(false)) if require_true => {
                findings.push(WorkflowValidationFinding::CoverageKeyMustBeTrue {
                    path: PathBuf::from(WORKFLOW_YAML),
                    key: (*key).to_string(),
                })
            }
            Some(JsonValue::Bool(false)) => {}
            Some(_) => findings.push(WorkflowValidationFinding::InvalidCoverageKeyType {
                path: PathBuf::from(WORKFLOW_YAML),
                key: (*key).to_string(),
            }),
            None => findings.push(WorkflowValidationFinding::MissingCoverageKey {
                path: PathBuf::from(WORKFLOW_YAML),
                key: (*key).to_string(),
            }),
        }
    }

    let markers = collect_test_coverage_markers(workflow_dir);
    for key in REQUIRED_MARKERS {
        if coverage.get(*key) == Some(&JsonValue::Bool(true)) && !markers.contains(*key) {
            findings.push(WorkflowValidationFinding::MissingCoverageMarker {
                path: PathBuf::from("src/tests"),
                key: (*key).to_string(),
            });
        }
    }
    if coverage.get("recovery") == Some(&JsonValue::Bool(true)) && !markers.contains("recovery") {
        findings.push(WorkflowValidationFinding::MissingCoverageMarker {
            path: PathBuf::from("src/tests"),
            key: "recovery".to_string(),
        });
    }

    findings
}

fn validate_validation_commands(
    spec: &crate::spec::WorkflowSpec,
) -> Vec<WorkflowValidationFinding> {
    match spec.validation.get("commands") {
        Some(JsonValue::Array(commands)) if !commands.is_empty() => {
            if !commands.iter().all(JsonValue::is_string) {
                return vec![WorkflowValidationFinding::InvalidValidationCommands {
                    path: PathBuf::from(WORKFLOW_YAML),
                }];
            }
            let command_strings = commands
                .iter()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>();
            let mut findings = Vec::new();
            if !command_strings.iter().any(|command| {
                let command = command.to_ascii_lowercase();
                command.contains("build")
                    || command.contains("typecheck")
                    || command.contains("tsc")
            }) {
                findings.push(WorkflowValidationFinding::MissingBuildValidationCommand {
                    path: PathBuf::from(WORKFLOW_YAML),
                });
            }
            if !command_strings
                .iter()
                .any(|command| command.to_ascii_lowercase().contains("test"))
            {
                findings.push(WorkflowValidationFinding::MissingTestValidationCommand {
                    path: PathBuf::from(WORKFLOW_YAML),
                });
            }
            findings
        }
        Some(JsonValue::Array(_)) => vec![WorkflowValidationFinding::EmptyValidationCommands {
            path: PathBuf::from(WORKFLOW_YAML),
        }],
        Some(_) => vec![WorkflowValidationFinding::InvalidValidationCommands {
            path: PathBuf::from(WORKFLOW_YAML),
        }],
        None => vec![WorkflowValidationFinding::MissingValidationCommands {
            path: PathBuf::from(WORKFLOW_YAML),
        }],
    }
}

fn validate_contract_smoke_metadata(
    spec: &crate::spec::WorkflowSpec,
) -> Vec<WorkflowValidationFinding> {
    let Some(smoke) = spec.validation.get("contractSmoke") else {
        return vec![WorkflowValidationFinding::MissingContractSmoke {
            path: PathBuf::from(WORKFLOW_YAML),
        }];
    };
    let valid = match smoke {
        JsonValue::Bool(true) => true,
        JsonValue::String(command) => !command.trim().is_empty(),
        JsonValue::Object(object) if object.get("enabled") != Some(&JsonValue::Bool(false)) => {
            object
                .get("command")
                .and_then(JsonValue::as_str)
                .is_none_or(|command| !command.trim().is_empty())
        }
        _ => false,
    };
    if valid {
        Vec::new()
    } else {
        vec![WorkflowValidationFinding::InvalidContractSmoke {
            path: PathBuf::from(WORKFLOW_YAML),
        }]
    }
}

fn validate_output_schemas(spec: &crate::spec::WorkflowSpec) -> Vec<WorkflowValidationFinding> {
    let mut findings = Vec::new();
    if let Some(output_schema) = spec.api.get("outputSchema")
        && !output_schema.is_null()
    {
        collect_ambiguous_output_schema_findings(
            output_schema,
            "api.outputSchema".to_string(),
            &mut findings,
        );
    }
    if let Some(tool) = spec.tool.as_ref()
        && !tool.output_schema.is_null()
    {
        collect_ambiguous_output_schema_findings(
            &tool.output_schema,
            "tool.outputSchema".to_string(),
            &mut findings,
        );
    }
    findings
}

fn collect_ambiguous_output_schema_findings(
    schema: &JsonValue,
    schema_path: String,
    findings: &mut Vec<WorkflowValidationFinding>,
) {
    if object_schema_has_ambiguous_shape(schema) {
        findings.push(WorkflowValidationFinding::AmbiguousWorkflowOutputSchema {
            path: PathBuf::from(WORKFLOW_YAML),
            schema_path: schema_path.clone(),
        });
    }

    for union_key in ["anyOf", "oneOf", "allOf"] {
        if let Some(entries) = schema.get(union_key).and_then(JsonValue::as_array) {
            for (index, item) in entries.iter().enumerate() {
                collect_ambiguous_output_schema_findings(
                    item,
                    format!("{schema_path}.{union_key}[{index}]"),
                    findings,
                );
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(JsonValue::as_object) {
        for (name, property_schema) in properties {
            collect_ambiguous_output_schema_findings(
                property_schema,
                format!("{schema_path}.properties.{name}"),
                findings,
            );
        }
    }
    if let Some(items) = schema.get("items") {
        collect_ambiguous_output_schema_findings(items, format!("{schema_path}.items"), findings);
    }
}

fn object_schema_has_ambiguous_shape(schema: &JsonValue) -> bool {
    if !schema_declares_object_type(schema) {
        return false;
    }
    let has_properties = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .is_some_and(|properties| !properties.is_empty());
    let has_additional_properties = schema
        .as_object()
        .is_some_and(|object| object.contains_key("additionalProperties"));
    !has_properties && !has_additional_properties
}

fn schema_declares_object_type(schema: &JsonValue) -> bool {
    match schema.get("type") {
        Some(JsonValue::String(type_name)) => type_name == "object",
        Some(JsonValue::Array(type_names)) => {
            type_names.iter().any(|type_name| type_name == "object")
        }
        _ => false,
    }
}

fn validate_runtime_state_gitignore(workflow_dir: &Path) -> Vec<WorkflowValidationFinding> {
    let gitignore_path = workflow_dir.join(".gitignore");
    let contents = fs::read_to_string(&gitignore_path).unwrap_or_default();
    let missing_patterns = RUNTIME_STATE_GITIGNORE_PATTERNS
        .iter()
        .filter(|pattern| !gitignore_contains_pattern(&contents, pattern))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if missing_patterns.is_empty() {
        Vec::new()
    } else {
        vec![WorkflowValidationFinding::RuntimeStateGitignoreMissing {
            path: PathBuf::from(".gitignore"),
            patterns: missing_patterns,
        }]
    }
}

fn gitignore_contains_pattern(contents: &str, pattern: &str) -> bool {
    contents.lines().any(|line| line.trim() == pattern)
}

fn validate_tracked_runtime_state(workflow_dir: &Path) -> Vec<WorkflowValidationFinding> {
    if !workflow_dir.join(".git/HEAD").is_file() {
        return Vec::new();
    }

    let Ok(output) = Command::new("git")
        .args(["ls-files", "-z", "--", "state", "artifacts"])
        .current_dir(workflow_dir)
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let mut tracked_runtime_paths = output
        .stdout
        .split(|byte| *byte == b'\0')
        .filter(|bytes| !bytes.is_empty())
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .map(PathBuf::from)
        .filter(|path| is_runtime_state_path(path))
        .collect::<Vec<_>>();
    tracked_runtime_paths.sort();
    if tracked_runtime_paths.is_empty() {
        Vec::new()
    } else {
        vec![WorkflowValidationFinding::TrackedRuntimeStateFiles {
            paths: tracked_runtime_paths,
        }]
    }
}

fn is_runtime_state_path(path: &Path) -> bool {
    path.starts_with(Path::new("artifacts"))
        || (path.starts_with(Path::new("state")) && path != Path::new("state/.gitkeep"))
}

fn collect_test_coverage_markers(workflow_dir: &Path) -> BTreeSet<String> {
    let mut markers = BTreeSet::new();
    for (_, _, contents) in workflow_test_files(workflow_dir) {
        for line in contents.lines() {
            let Some(rest) = line.trim_start().strip_prefix("//") else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix(WORKFLOW_TEST_MARKER_PREFIX) else {
                continue;
            };
            for marker in rest
                .split(|ch: char| ch.is_whitespace() || ch == ',')
                .map(str::trim)
                .filter(|marker| !marker.is_empty())
            {
                markers.insert(marker.to_string());
            }
        }
    }
    markers
}

fn collect_layout_issues(
    workflow_dir: &Path,
    dir: &Path,
    code_outside_src: &mut Vec<PathBuf>,
    tests_outside_src_tests: &mut Vec<PathBuf>,
    databases_outside_state: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_layout_dir(&path) {
                continue;
            }
            collect_layout_issues(
                workflow_dir,
                &path,
                code_outside_src,
                tests_outside_src_tests,
                databases_outside_state,
            );
            continue;
        }

        let Ok(relative) = path.strip_prefix(workflow_dir) else {
            continue;
        };

        if is_database_file(relative) && !relative.starts_with(Path::new("state")) {
            databases_outside_state.push(relative.to_path_buf());
            continue;
        }
        if is_test_file(relative) {
            if !relative.starts_with(Path::new("src/tests")) {
                tests_outside_src_tests.push(relative.to_path_buf());
            }
            continue;
        }
        if is_code_file(relative)
            && !relative.starts_with(Path::new("src"))
            && !is_allowed_non_src_code_file(relative)
        {
            code_outside_src.push(relative.to_path_buf());
        }
    }
}

fn workflow_test_files(workflow_dir: &Path) -> Vec<(PathBuf, PathBuf, String)> {
    let mut files = Vec::new();
    visit_workflow_files(workflow_dir, workflow_dir, &mut |relative, path| {
        if is_test_file(relative)
            && let Ok(contents) = fs::read_to_string(path)
        {
            files.push((relative.to_path_buf(), path.to_path_buf(), contents));
        }
    });
    files
}

fn visit_workflow_files(workflow_dir: &Path, dir: &Path, visitor: &mut impl FnMut(&Path, &Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_layout_dir(&path) {
                continue;
            }
            visit_workflow_files(workflow_dir, &path, visitor);
            continue;
        }
        let Ok(relative) = path.strip_prefix(workflow_dir) else {
            continue;
        };
        visitor(relative, &path);
    }
}

fn should_skip_layout_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".git" | "node_modules" | "target" | "dist" | "build" | "coverage")
    )
}

fn is_code_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts")
    )
}

fn is_test_file(path: &Path) -> bool {
    if path.starts_with(Path::new("tests")) {
        return true;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".test.") || name.contains(".spec."))
}

fn is_database_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    file_name.ends_with(".db")
        || file_name.ends_with(".sqlite")
        || file_name.ends_with(".sqlite3")
        || file_name.ends_with(".db-wal")
        || file_name.ends_with(".db-shm")
        || file_name.ends_with(".sqlite-wal")
        || file_name.ends_with(".sqlite-shm")
        || file_name.ends_with(".sqlite3-wal")
        || file_name.ends_with(".sqlite3-shm")
}

fn is_allowed_non_src_code_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    path.parent().is_some_and(|parent| parent == Path::new(""))
        && matches!(
            file_name,
            name if name.ends_with(".config.ts")
                || name.ends_with(".config.tsx")
                || name.ends_with(".config.js")
                || name.ends_with(".config.jsx")
                || name.ends_with(".config.mjs")
                || name.ends_with(".config.cjs")
                || name.ends_with(".config.mts")
                || name.ends_with(".config.cts")
        )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::TempDir;

    use super::validate_workflow_dir;
    use crate::registry::WorkflowValidationStatus;
    use crate::spec::WorkflowSpec;
    use crate::spec::write_workflow_spec;
    use crate::validation_finding::finding_messages;

    fn create_valid_workflow_dir(root: &TempDir, id: &str) -> std::path::PathBuf {
        let workflow_dir = root.path().join(id);
        fs::create_dir_all(workflow_dir.join("src/tests")).unwrap();
        fs::create_dir_all(workflow_dir.join("state")).unwrap();
        fs::create_dir_all(workflow_dir.join(".git")).unwrap();
        fs::write(
            workflow_dir.join(".gitignore"),
            "node_modules/\nartifacts/\nstate/*\n!state/.gitkeep\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("README.md"),
            "# Example\n\n## Usage\n\nRun `/example`.\n\n## Workflow Runtime\n\nRuns as a local TypeScript workflow.\n\n## Dependencies\n\nUses local package dependencies only.\n\n## Validation\n\nRun the configured build and test commands.\n\n## Maintenance\n\nKeep docs, metadata, and tests aligned.\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("DESIGN.md"),
            "# Design\n\n## Overview\n\nExample workflow.\n\n## Architecture\n\nTypeScript source under src/.\n\n## Data Flow\n\nInput becomes output.\n\n## Failure Handling\n\nInvalid input is rejected.\n\n## Recovery Behavior\n\nNo recovery behavior.\n\n## Test Matrix\n\nPositive, negative, load, and autocomplete tests.\n\n## Maintenance Notes\n\nKeep validation metadata current.\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "codex-workflow-example",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "tsc --noEmit",
    "test": "node --test src/tests/*.test.ts",
    "run": "node src/workflow.ts"
  },
  "dependencies": {
    "@openai/codex-sdk": "latest"
  },
  "devDependencies": {
    "@types/node": "latest",
    "typescript": "latest"
  }
}
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "noEmit": true
  },
  "include": ["src/**/*.ts"]
}
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            r##"import { WorkflowContext } from "@openai/codex-sdk/workflow";

export interface WorkflowInput { input?: string; }

export interface WorkflowOutput { ok: boolean; input: WorkflowInput; }

export const WorkflowOutput = {
  toTuiMarkdown(_result: WorkflowOutput) {
    return { markdown: "# Example\n\nDone." };
  },
};

export default async function example(ctx: WorkflowContext, input: WorkflowInput): Promise<WorkflowOutput> {
  ctx.progress("Running workflow", { input });
  return { ok: true, input };
}

export async function complete() {
  return [];
}
"##,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.positive.test.ts"),
            r##"// workflow-covers: positive progress finalResult
import assert from "node:assert/strict";
import test from "node:test";
import workflow, { WorkflowOutput } from "../workflow.js";

test("workflow completes successfully", async () => {
  const events: Array<[string, unknown]> = [];
  const output = await workflow({
    progress(message, data) {
      events.push([message, data]);
    },
    reportToUserMarkdown(markdown) {
      events.push([markdown, null]);
    },
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
    status() {},
    runWorkflow() { throw new Error("runWorkflow() is unavailable in unit tests"); },
  } as never, { input: "example" });
  const formatted = WorkflowOutput.toTuiMarkdown(output);

  assert.deepEqual(output, { ok: true, input: { input: "example" } });
  assert.deepEqual(formatted, { markdown: "# Example\n\nDone." });
  assert.equal(events.length, 1);
});
"##,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "// workflow-covers: load\nexport {};\n",
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
            r#"// workflow-covers: autocomplete
import assert from "node:assert/strict";
import test from "node:test";
import { complete } from "../workflow.js";

test("workflow exposes complete", async () => {
  const suggestions = await complete({
    cwd: process.cwd(),
    currentWorkingDirectory: process.cwd(),
    repoRoot: process.cwd(),
    workingDirectory: process.cwd(),
    progress() {},
    status() {},
    reportToUserMarkdown() {},
    runWorkflow() { throw new Error("runWorkflow() is unavailable in unit tests"); },
  } as never, { argv: [], text: "" });

  assert.deepEqual(suggestions, []);
});
"#,
        )
        .unwrap();
        fs::write(
            workflow_dir.join("src/tests/workflow.negative.test.ts"),
            r#"// workflow-covers: negative failureUx
import assert from "node:assert/strict";
import test from "node:test";
import workflow from "../workflow.js";

test("workflow rejects invalid input", async () => {
  await assert.rejects(
    workflow({
      progress() {},
      reportToUserMarkdown() {},
      cwd: process.cwd(),
      currentWorkingDirectory: process.cwd(),
      repoRoot: process.cwd(),
      workingDirectory: process.cwd(),
      status() {},
      runWorkflow() { throw new Error("runWorkflow() is unavailable in unit tests"); },
    } as never, null),
    /workflow input must be a JSON object/
  );
});
"#,
        )
        .unwrap();
        fs::write(workflow_dir.join("state/.gitkeep"), "").unwrap();
        write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &WorkflowSpec {
                id: id.to_string(),
                dependencies: json!({
                    "runtime": ["@openai/codex-sdk"],
                    "development": ["@types/node", "typescript"],
                }),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
                    "contractSmoke": { "input": { "input": "example" } },
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();
        workflow_dir
    }

    #[test]
    fn validate_workflow_dir_accepts_complete_workflow() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Valid);
        assert!(validation.findings.is_empty());
    }

    #[test]
    fn validate_workflow_dir_reports_missing_design_doc() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::remove_file(workflow_dir.join("DESIGN.md")).unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(finding_messages(&validation.findings).contains(&"missing DESIGN.md".to_string()));
    }

    #[test]
    fn validate_workflow_dir_reports_missing_load_marker() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::write(
            workflow_dir.join("src/tests/workflow.load.test.ts"),
            "export {};\n",
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| {
                    message.contains("missing test coverage marker `// workflow-covers: load`")
                })
        );
    }

    #[test]
    fn validate_workflow_dir_rejects_ambiguous_output_schema() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &WorkflowSpec {
                id: "example".to_string(),
                api: json!({
                    "inputSchema": { "type": "object", "additionalProperties": true },
                    "outputSchema": { "type": "object" }
                }),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| {
                    message.contains(
                        "workflow output schema at api.outputSchema must declare properties or additionalProperties explicitly",
                    )
                })
        );
    }

    #[test]
    fn validate_workflow_dir_accepts_explicit_open_output_schema() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &WorkflowSpec {
                id: "example".to_string(),
                dependencies: json!({
                    "runtime": ["@openai/codex-sdk"],
                    "development": ["@types/node", "typescript"],
                }),
                api: json!({
                    "inputSchema": { "type": "object", "additionalProperties": true },
                    "outputSchema": { "type": "object", "additionalProperties": true }
                }),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
                    "contractSmoke": { "input": { "input": "example" } },
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Valid);
        assert!(validation.findings.is_empty());
    }

    #[test]
    fn validate_workflow_dir_requires_runtime_state_gitignore_patterns() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::write(workflow_dir.join(".gitignore"), "node_modules/\n").unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| message.contains("runtime state ignore rules are missing"))
        );
    }

    #[test]
    fn validate_workflow_dir_rejects_tracked_runtime_state() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::remove_dir_all(workflow_dir.join(".git")).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .current_dir(&workflow_dir)
                .status()
                .unwrap()
                .success()
        );
        fs::write(workflow_dir.join("state/reviews.sqlite3"), "db").unwrap();
        fs::create_dir_all(workflow_dir.join("artifacts/run-1")).unwrap();
        fs::write(workflow_dir.join("artifacts/run-1/output.txt"), "artifact").unwrap();
        assert!(
            Command::new("git")
                .args([
                    "add",
                    "-f",
                    "state/reviews.sqlite3",
                    "artifacts/run-1/output.txt",
                ])
                .current_dir(&workflow_dir)
                .status()
                .unwrap()
                .success()
        );

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| {
                    message.contains(
                        "runtime state files must not be tracked by git: artifacts/run-1/output.txt, state/reviews.sqlite3",
                    )
                })
        );
    }

    #[test]
    fn validate_workflow_dir_reports_missing_autocomplete_marker() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::write(
            workflow_dir.join("src/tests/workflow.autocomplete.test.ts"),
            "export {};\n",
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| {
                    message
                        .contains("missing test coverage marker `// workflow-covers: autocomplete`")
                })
        );
    }

    #[test]
    fn validate_workflow_dir_reports_dependency_metadata_drift_and_unused_runtime_deps() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::write(
            workflow_dir.join("package.json"),
            r#"{
  "name": "codex-workflow-example",
  "private": true,
  "type": "module",
  "scripts": {
    "build": "tsc --noEmit",
    "test": "node --test src/tests/*.test.ts",
    "run": "node src/workflow.ts"
  },
  "dependencies": {
    "@modelcontextprotocol/sdk": "latest",
    "@openai/codex-sdk": "latest"
  },
  "devDependencies": {
    "@types/node": "latest",
    "typescript": "latest"
  }
}
"#,
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert_eq!(
            finding_messages(&validation.findings),
            vec![
                "package.json declares unused runtime dependency `@modelcontextprotocol/sdk`"
                    .to_string(),
                "package `@modelcontextprotocol/sdk` is listed in package.json dependencies but missing from workflow.yaml dependencies.runtime".to_string(),
            ]
        );
    }

    #[test]
    fn validate_workflow_dir_requires_contract_smoke_and_build_test_commands() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &WorkflowSpec {
                id: "example".to_string(),
                dependencies: json!({
                    "runtime": ["@openai/codex-sdk"],
                    "development": ["@types/node", "typescript"],
                }),
                validation: json!({
                    "commands": ["echo ok"],
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": false,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert_eq!(
            finding_messages(&validation.findings),
            vec![
                "validation commands must include a build/typecheck step".to_string(),
                "validation commands must include a test step".to_string(),
                "validation.contractSmoke must be configured".to_string(),
            ]
        );
    }

    #[test]
    fn validate_workflow_dir_reports_undeclared_imports() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        fs::write(
            workflow_dir.join("src/workflow.ts"),
            r#"import leftPad from "left-pad";

export default { async run() { return leftPad("x", 2); } };
"#,
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| { message.contains("imports undeclared package `left-pad`") })
        );
    }

    #[test]
    fn validate_workflow_dir_requires_recovery_marker_when_declared() {
        let root = TempDir::new().unwrap();
        let workflow_dir = create_valid_workflow_dir(&root, "example");
        write_workflow_spec(
            &workflow_dir.join(crate::spec::WORKFLOW_YAML),
            &WorkflowSpec {
                id: "example".to_string(),
                validation: json!({
                    "commands": ["npm run build", "npm test"],
                    "coverage": {
                        "positive": true,
                        "negative": true,
                        "progress": true,
                        "finalResult": true,
                        "failureUx": true,
                        "load": true,
                        "autocomplete": true,
                        "recovery": true,
                    }
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let validation = validate_workflow_dir(root.path(), &workflow_dir, "example");

        assert_eq!(validation.status, WorkflowValidationStatus::Invalid);
        assert!(
            finding_messages(&validation.findings)
                .iter()
                .any(|message| {
                    message.contains("missing test coverage marker `// workflow-covers: recovery`")
                })
        );
    }
}
