use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value as JsonValue;

use crate::spec::WORKFLOW_YAML;
use crate::validation_finding::WorkflowValidationFinding;

const REQUIRED_PACKAGE_SCRIPTS: &[&str] = &["build", "test", "run"];
const BARE_NODE_BUILTINS: &[&str] = &[
    "assert",
    "assert/strict",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "dns/promises",
    "domain",
    "events",
    "fs",
    "fs/promises",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "path/posix",
    "path/win32",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "readline/promises",
    "repl",
    "stream",
    "stream/consumers",
    "stream/promises",
    "stream/web",
    "string_decoder",
    "sys",
    "timers",
    "timers/promises",
    "tls",
    "trace_events",
    "tty",
    "url",
    "util",
    "util/types",
    "v8",
    "vm",
    "wasi",
    "worker_threads",
    "zlib",
];

static IMPORT_FROM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?ms)^\s*import\b[\s\S]*?\bfrom\s+['\"]([^'\"]+)['\"]"#)
        .unwrap_or_else(|err| panic!("invalid import regex: {err}"))
});
static SIDE_EFFECT_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*import\s+['\"]([^'\"]+)['\"]"#)
        .unwrap_or_else(|err| panic!("invalid import regex: {err}"))
});
static EXPORT_FROM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?ms)^\s*export\b[\s\S]*?\bfrom\s+['\"]([^'\"]+)['\"]"#)
        .unwrap_or_else(|err| panic!("invalid export regex: {err}"))
});
static DYNAMIC_IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\(\s*['\"]([^'\"]+)['\"]\s*\)"#)
        .unwrap_or_else(|err| panic!("invalid dynamic import regex: {err}"))
});
static REQUIRE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"require\(\s*['\"]([^'\"]+)['\"]\s*\)"#)
        .unwrap_or_else(|err| panic!("invalid require regex: {err}"))
});

pub(crate) fn validate_package_manifest(
    workflow_dir: &Path,
    spec: Option<&crate::spec::WorkflowSpec>,
) -> Vec<WorkflowValidationFinding> {
    let package_json_path = workflow_dir.join("package.json");
    let package_json = match read_package_manifest(&package_json_path) {
        Ok(Some(value)) => value,
        Ok(None) => return Vec::new(),
        Err(finding) => return vec![finding],
    };
    let mut findings = Vec::new();
    validate_package_manifest_shape(&package_json, &mut findings);
    validate_unused_runtime_dependencies(workflow_dir, &package_json, &mut findings);
    if let Some(spec) = spec {
        validate_dependency_metadata(spec, &package_json, &mut findings);
    }
    findings
}

pub(crate) fn validate_local_package_imports(
    workflow_dir: &Path,
) -> Vec<WorkflowValidationFinding> {
    let package_json_path = workflow_dir.join("package.json");
    let package_json_value = match read_package_manifest(&package_json_path) {
        Ok(Some(value)) => value,
        Ok(None) | Err(_) => return Vec::new(),
    };
    let declared_packages = declared_packages(&package_json_value);

    let mut findings = Vec::new();
    for (relative, _path, contents) in workflow_code_files(workflow_dir) {
        let specifiers = imported_specifiers(&contents);
        for specifier in specifiers {
            if is_builtin_specifier(&specifier) {
                continue;
            }
            if let Some(package_name) = package_name_from_specifier(&specifier)
                && !declared_packages.contains(&package_name)
            {
                findings.push(WorkflowValidationFinding::UndeclaredPackageImport {
                    path: relative.clone(),
                    specifier,
                    package_name,
                });
            }
        }
    }
    findings.sort_by_key(WorkflowValidationFinding::message);
    findings.dedup();
    findings
}

fn read_package_manifest(
    package_json_path: &Path,
) -> Result<Option<JsonValue>, WorkflowValidationFinding> {
    let Ok(package_json) = fs::read_to_string(package_json_path) else {
        return Ok(None);
    };
    serde_json::from_str::<JsonValue>(&package_json)
        .map(Some)
        .map_err(
            |err| WorkflowValidationFinding::PackageManifestParseFailed {
                path: package_json_path.to_path_buf(),
                error: err.to_string(),
            },
        )
}

fn validate_package_manifest_shape(
    package_json: &JsonValue,
    findings: &mut Vec<WorkflowValidationFinding>,
) {
    let path = PathBuf::from("package.json");
    let Some(object) = package_json.as_object() else {
        findings.push(WorkflowValidationFinding::InvalidPackageManifestField {
            path,
            field: "$".to_string(),
            expected: "a JSON object".to_string(),
        });
        return;
    };

    match object.get("name").and_then(JsonValue::as_str) {
        Some(name) if name.starts_with("codex-workflow-") => {}
        _ => findings.push(WorkflowValidationFinding::InvalidPackageManifestField {
            path: path.clone(),
            field: "name".to_string(),
            expected: "a `codex-workflow-*` package name".to_string(),
        }),
    }
    if object.get("private") != Some(&JsonValue::Bool(true)) {
        findings.push(WorkflowValidationFinding::InvalidPackageManifestField {
            path: path.clone(),
            field: "private".to_string(),
            expected: "`true`".to_string(),
        });
    }
    if object.get("type").and_then(JsonValue::as_str) != Some("module") {
        findings.push(WorkflowValidationFinding::InvalidPackageManifestField {
            path: path.clone(),
            field: "type".to_string(),
            expected: "`module`".to_string(),
        });
    }

    let scripts = object.get("scripts").and_then(JsonValue::as_object);
    for script in REQUIRED_PACKAGE_SCRIPTS {
        if scripts
            .and_then(|scripts| scripts.get(*script))
            .and_then(JsonValue::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            findings.push(WorkflowValidationFinding::MissingPackageScript {
                path: path.clone(),
                script: (*script).to_string(),
            });
        }
    }

    for field in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if object.get(field).is_some_and(|value| !value.is_object()) {
            findings.push(WorkflowValidationFinding::InvalidPackageManifestField {
                path: path.clone(),
                field: field.to_string(),
                expected: "an object".to_string(),
            });
        }
    }
}

fn validate_unused_runtime_dependencies(
    workflow_dir: &Path,
    package_json: &JsonValue,
    findings: &mut Vec<WorkflowValidationFinding>,
) {
    let imported_packages = workflow_code_files(workflow_dir)
        .into_iter()
        .flat_map(|(_, _, contents)| imported_specifiers(&contents))
        .filter(|specifier| !is_builtin_specifier(specifier))
        .filter_map(|specifier| package_name_from_specifier(&specifier))
        .collect::<BTreeSet<_>>();
    let script_text = package_scripts_text(package_json);
    for package_name in package_dependency_names(package_json, "dependencies") {
        if imported_packages.contains(&package_name)
            || package_used_by_scripts(&package_name, &script_text)
        {
            continue;
        }
        findings.push(WorkflowValidationFinding::UnusedPackageDependency {
            path: PathBuf::from("package.json"),
            package_name,
        });
    }
}

fn validate_dependency_metadata(
    spec: &crate::spec::WorkflowSpec,
    package_json: &JsonValue,
    findings: &mut Vec<WorkflowValidationFinding>,
) {
    for (package_json_field, workflow_yaml_field) in [
        ("dependencies", "runtime"),
        ("devDependencies", "development"),
    ] {
        let package_names = package_dependency_names(package_json, package_json_field);
        let metadata_names =
            workflow_dependency_names(spec, workflow_yaml_field, findings).unwrap_or_default();

        for package_name in package_names.difference(&metadata_names) {
            findings.push(
                WorkflowValidationFinding::WorkflowDependencyMetadataMismatch {
                    path: PathBuf::from(WORKFLOW_YAML),
                    package_name: package_name.clone(),
                    source: format!("package.json {package_json_field}"),
                    target: format!("workflow.yaml dependencies.{workflow_yaml_field}"),
                },
            );
        }
        for package_name in metadata_names.difference(&package_names) {
            findings.push(
                WorkflowValidationFinding::WorkflowDependencyMetadataMismatch {
                    path: PathBuf::from(WORKFLOW_YAML),
                    package_name: package_name.clone(),
                    source: format!("workflow.yaml dependencies.{workflow_yaml_field}"),
                    target: format!("package.json {package_json_field}"),
                },
            );
        }
    }
}

fn workflow_dependency_names(
    spec: &crate::spec::WorkflowSpec,
    field: &str,
    findings: &mut Vec<WorkflowValidationFinding>,
) -> Option<BTreeSet<String>> {
    let Some(dependencies) = spec.dependencies.as_object() else {
        if !spec.dependencies.is_null() {
            findings.push(
                WorkflowValidationFinding::InvalidWorkflowDependencyMetadata {
                    path: PathBuf::from(WORKFLOW_YAML),
                    field: "dependencies".to_string(),
                },
            );
        }
        return Some(BTreeSet::new());
    };
    let Some(value) = dependencies.get(field) else {
        return Some(BTreeSet::new());
    };
    let Some(entries) = value.as_array() else {
        findings.push(
            WorkflowValidationFinding::InvalidWorkflowDependencyMetadata {
                path: PathBuf::from(WORKFLOW_YAML),
                field: format!("dependencies.{field}"),
            },
        );
        return None;
    };
    let mut names = BTreeSet::new();
    for entry in entries {
        let Some(name) = entry.as_str() else {
            findings.push(
                WorkflowValidationFinding::InvalidWorkflowDependencyMetadata {
                    path: PathBuf::from(WORKFLOW_YAML),
                    field: format!("dependencies.{field}"),
                },
            );
            return None;
        };
        names.insert(name.to_string());
    }
    Some(names)
}

fn package_dependency_names(package_json: &JsonValue, field: &str) -> BTreeSet<String> {
    package_json
        .get(field)
        .and_then(JsonValue::as_object)
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default()
}

fn package_scripts_text(package_json: &JsonValue) -> String {
    package_json
        .get("scripts")
        .and_then(JsonValue::as_object)
        .map(|scripts| {
            scripts
                .values()
                .filter_map(JsonValue::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn package_used_by_scripts(package_name: &str, script_text: &str) -> bool {
    script_text.contains(package_name)
        || match package_name {
            "typescript" => script_text.contains("tsc"),
            "tsx" => script_text.contains("tsx"),
            _ => false,
        }
}

fn declared_packages(package_json: &JsonValue) -> BTreeSet<String> {
    let mut packages = BTreeSet::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        if let Some(entries) = package_json.get(key).and_then(JsonValue::as_object) {
            packages.extend(entries.keys().cloned());
        }
    }
    packages
}

fn imported_specifiers(contents: &str) -> BTreeSet<String> {
    let mut specifiers = BTreeSet::new();
    for regex in [
        &*IMPORT_FROM_RE,
        &*SIDE_EFFECT_IMPORT_RE,
        &*EXPORT_FROM_RE,
        &*DYNAMIC_IMPORT_RE,
        &*REQUIRE_RE,
    ] {
        for captures in regex.captures_iter(contents) {
            if let Some(specifier) = captures.get(1) {
                specifiers.insert(specifier.as_str().to_string());
            }
        }
    }
    specifiers
}

fn package_name_from_specifier(specifier: &str) -> Option<String> {
    if specifier.starts_with('.') || specifier.starts_with('/') || specifier.starts_with("node:") {
        return None;
    }
    if let Some(rest) = specifier.strip_prefix('@') {
        let mut segments = rest.split('/');
        let scope = segments.next()?;
        let name = segments.next()?;
        return Some(format!("@{scope}/{name}"));
    }
    Some(specifier.split('/').next().unwrap_or(specifier).to_string())
}

fn is_builtin_specifier(specifier: &str) -> bool {
    if specifier.starts_with("node:") {
        return true;
    }
    BARE_NODE_BUILTINS.contains(&specifier)
}

fn workflow_code_files(workflow_dir: &Path) -> Vec<(PathBuf, PathBuf, String)> {
    let mut files = Vec::new();
    visit_workflow_files(workflow_dir, workflow_dir, &mut |relative, path| {
        if is_code_file(relative)
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
