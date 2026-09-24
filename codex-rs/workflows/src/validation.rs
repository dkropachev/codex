use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;
use tree_sitter::Node;
use tree_sitter::Parser;
use tree_sitter::Tree;

use crate::WorkflowPackage;
use crate::manifest::MAX_WORKFLOW_SOURCE_BYTES;
use crate::manifest::MAX_WORKFLOW_YAML_BYTES;
use crate::manifest::first_legacy_field;
use crate::manifest::is_package_regular_directory;
use crate::manifest::is_package_regular_file;
use crate::manifest::legacy_migration_message;
use crate::manifest::read_bounded_utf8;

mod checks;

use checks::validate_commands;
use checks::validate_coverage;
use checks::validate_git_layout;
use checks::validate_gitignore;

const REQUIRED_FILES: &[&str] = &[
    "workflow.yaml",
    "package.json",
    ".gitignore",
    "README.md",
    "DESIGN.md",
    "src/workflow.ts",
    "state/.gitkeep",
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ValidationFinding {
    pub code: String,
    pub message: String,
}

impl ValidationFinding {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub findings: Vec<ValidationFinding>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn render(&self) -> String {
        self.findings
            .iter()
            .map(|finding| format!("{}: {}", finding.code, finding.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn validate_workflow(root: &Path) -> ValidationReport {
    let mut findings = BTreeSet::new();
    validate_required_layout(root, &mut findings);
    validate_legacy_metadata(root, &mut findings);

    let package = match WorkflowPackage::load(root) {
        Ok(package) => Some(package),
        Err(err) => {
            findings.insert(ValidationFinding::new("metadata", format!("{err:#}")));
            None
        }
    };
    if let Some(package) = package.as_ref() {
        validate_package_json(package, &mut findings);
        let sources = match crate::runner::scan_workflow_sources(&package.root) {
            Ok(sources) => sources,
            Err(err) => {
                findings.insert(ValidationFinding::new(
                    "load",
                    format!("workflow source scan failed: {err:#}"),
                ));
                Vec::new()
            }
        };
        validate_module_load(package, &mut findings);
        validate_source_contract(package, &sources, &mut findings);
        validate_coverage(package, &mut findings);
        validate_commands(package, &mut findings);
    }
    validate_gitignore(root, &mut findings);
    validate_git_layout(root, &mut findings);

    ValidationReport {
        findings: findings.into_iter().collect(),
    }
}

pub(crate) fn validate_executable_package(package: &WorkflowPackage) -> anyhow::Result<()> {
    let mut findings = BTreeSet::new();
    validate_required_layout(&package.root, &mut findings);
    validate_package_json(package, &mut findings);
    match crate::runner::scan_workflow_sources(&package.root) {
        Ok(sources) => validate_source_contract(package, &sources, &mut findings),
        Err(err) => {
            findings.insert(ValidationFinding::new(
                "load",
                format!("workflow source scan failed: {err:#}"),
            ));
        }
    }
    validate_coverage(package, &mut findings);
    validate_gitignore(&package.root, &mut findings);
    validate_git_layout(&package.root, &mut findings);
    if findings.is_empty() {
        Ok(())
    } else {
        let report = ValidationReport {
            findings: findings.into_iter().collect(),
        };
        anyhow::bail!(
            "workflow package does not satisfy the canonical v1 contract:\n{}",
            report.render()
        )
    }
}

fn validate_module_load(package: &WorkflowPackage, findings: &mut BTreeSet<ValidationFinding>) {
    match crate::schema::load_workflow_contract(&package.root, &package.manifest) {
        Ok(_) => {}
        Err(err) => {
            findings.insert(ValidationFinding::new(
                "load",
                format!("workflow module failed canonical import validation: {err:#}"),
            ));
        }
    }
}

fn validate_required_layout(root: &Path, findings: &mut BTreeSet<ValidationFinding>) {
    for relative in REQUIRED_FILES {
        if !is_package_regular_file(root, Path::new(relative)) {
            findings.insert(ValidationFinding::new(
                "layout",
                format!("missing or non-regular required package file `{relative}`"),
            ));
        }
    }
    if !is_package_regular_directory(root, Path::new("src/tests")) {
        findings.insert(ValidationFinding::new(
            "layout",
            "missing required package directory `src/tests`",
        ));
    }
    if !is_package_regular_directory(root, Path::new("state")) {
        findings.insert(ValidationFinding::new(
            "layout",
            "missing required package directory `state`",
        ));
    }
    if !is_package_regular_directory(root, Path::new(".git")) {
        findings.insert(ValidationFinding::new(
            "layout",
            "workflow package is not initialized as a git repository",
        ));
    }
}

fn validate_legacy_metadata(root: &Path, findings: &mut BTreeSet<ValidationFinding>) {
    let path = root.join("workflow.yaml");
    if !is_package_regular_file(root, Path::new("workflow.yaml")) {
        return;
    }
    let Ok(contents) = read_bounded_utf8(&path, MAX_WORKFLOW_YAML_BYTES) else {
        return;
    };
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&contents) else {
        return;
    };
    if let Some(field) = first_legacy_field(&value) {
        findings.insert(ValidationFinding::new(
            "legacy",
            legacy_migration_message(field),
        ));
    }
}

fn validate_package_json(package: &WorkflowPackage, findings: &mut BTreeSet<ValidationFinding>) {
    let Some(object) = package.package_json.as_object() else {
        return;
    };
    if object.get("private") != Some(&Value::Bool(true)) {
        findings.insert(ValidationFinding::new(
            "package",
            "package.json must set `private` to true",
        ));
    }
    if object.get("type").and_then(Value::as_str) != Some("module") {
        findings.insert(ValidationFinding::new(
            "package",
            "package.json must set `type` to `module`",
        ));
    }

    let dependencies = dependency_map(object, "dependencies", findings);
    let dev_dependencies = dependency_map(object, "devDependencies", findings);
    for (name, specifier) in dependencies.iter().chain(dev_dependencies.iter()) {
        let Some(installed_path) = local_dependency_path(&package.root, name) else {
            findings.insert(ValidationFinding::new(
                "dependency",
                format!("dependency name `{name}` is invalid"),
            ));
            continue;
        };
        if !is_contained_directory(&package.root, &installed_path) {
            findings.insert(ValidationFinding::new(
                "dependency",
                format!(
                    "dependency `{name}` is declared but not installed in the workflow's local node_modules"
                ),
            ));
        }
        let Some(relative) = specifier.strip_prefix("file:") else {
            continue;
        };
        let dependency_path = package.root.join(relative);
        let escapes_package = Path::new(relative).is_absolute()
            || Path::new(relative)
                .components()
                .any(|component| component == std::path::Component::ParentDir)
            || dependency_path
                .canonicalize()
                .ok()
                .zip(package.root.canonicalize().ok())
                .is_some_and(|(dependency_path, package_root)| {
                    !dependency_path.starts_with(package_root)
                });
        if escapes_package {
            findings.insert(ValidationFinding::new(
                "dependency",
                format!("dependency `{name}` resolves outside the workflow package"),
            ));
        } else if !dependency_path.exists() {
            findings.insert(ValidationFinding::new(
                "dependency",
                format!(
                    "local dependency `{name}` does not exist at {}",
                    dependency_path.display()
                ),
            ));
        }
    }
}

fn is_contained_directory(root: &Path, path: &Path) -> bool {
    path.canonicalize()
        .ok()
        .zip(root.canonicalize().ok())
        .is_some_and(|(path, root)| path.starts_with(root) && path.is_dir())
}

fn local_dependency_path(root: &Path, name: &str) -> Option<std::path::PathBuf> {
    let parts = name.split('/').collect::<Vec<_>>();
    let valid_part = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    match parts.as_slice() {
        [name] if !name.starts_with('@') && valid_part(name) => {
            Some(root.join("node_modules").join(name))
        }
        [scope, name]
            if scope.starts_with('@')
                && valid_part(scope.trim_start_matches('@'))
                && valid_part(name) =>
        {
            Some(root.join("node_modules").join(scope).join(name))
        }
        _ => None,
    }
}

fn dependency_map(
    package: &serde_json::Map<String, Value>,
    key: &str,
    findings: &mut BTreeSet<ValidationFinding>,
) -> BTreeMap<String, String> {
    let Some(value) = package.get(key) else {
        findings.insert(ValidationFinding::new(
            "package",
            format!("package.json must declare an empty or populated `{key}` object"),
        ));
        return BTreeMap::new();
    };
    let Some(object) = value.as_object() else {
        findings.insert(ValidationFinding::new(
            "package",
            format!("package.json `{key}` must be an object"),
        ));
        return BTreeMap::new();
    };
    object
        .iter()
        .filter_map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .or_else(|| {
                    findings.insert(ValidationFinding::new(
                        "package",
                        format!("dependency `{name}` must have a string specifier"),
                    ));
                    None
                })
        })
        .collect()
}

fn validate_source_contract(
    package: &WorkflowPackage,
    sources: &[crate::runner::SourceInspection],
    findings: &mut BTreeSet<ValidationFinding>,
) {
    let path = package.root.join("src/workflow.ts");
    let Ok(source) = read_bounded_utf8(&path, MAX_WORKFLOW_SOURCE_BYTES) else {
        return;
    };
    let syntax = parse_typescript(&source, &path);
    for label in ["WorkflowInput", "WorkflowOutput"] {
        if !syntax
            .as_ref()
            .is_some_and(|tree| tree_exports_type(tree, &source, label))
        {
            findings.insert(ValidationFinding::new(
                "export",
                format!("src/workflow.ts is missing the `{label}` export"),
            ));
        }
    }
    let runtime_exports = sources
        .iter()
        .find(|source| source.path == "src/workflow.ts")
        .map(|source| source.exports.as_slice())
        .unwrap_or_default();
    for label in ["inputSchema", "outputSchema"] {
        if !runtime_exports.iter().any(|name| name == label) {
            findings.insert(ValidationFinding::new(
                "export",
                format!("src/workflow.ts is missing the `{label}` export"),
            ));
        }
    }
    if !syntax
        .as_ref()
        .is_some_and(|tree| tree_exports_default_define_workflow(tree, &source))
    {
        findings.insert(ValidationFinding::new(
            "export",
            "src/workflow.ts is missing the `default defineWorkflow` export",
        ));
    }

    let declared = package
        .package_json
        .as_object()
        .into_iter()
        .flat_map(|object| ["dependencies", "devDependencies"].map(|key| object.get(key)))
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|dependencies| dependencies.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for source in sources {
        let source_path = package.root.join(&source.path);
        let source_text =
            read_bounded_utf8(&source_path, MAX_WORKFLOW_SOURCE_BYTES).unwrap_or_default();
        if parse_typescript(&source_text, &source_path)
            .is_some_and(|tree| tree_has_nonliteral_dependency_call(&tree, &source_text))
        {
            findings.insert(ValidationFinding::new(
                "dependency",
                format!(
                    "{} contains a non-literal dynamic import or require",
                    source_path.display()
                ),
            ));
        }
        for source_import in &source.imports {
            let specifier = source_import.path.as_str();
            if source_import.builtin
                || specifier == "bun"
                || specifier.starts_with("bun:")
                || specifier.starts_with("node:")
            {
                continue;
            }
            if specifier.starts_with('.') {
                validate_local_import(package, &source_path, specifier, findings);
                continue;
            }
            if specifier.starts_with('/') {
                findings.insert(ValidationFinding::new(
                    "dependency",
                    format!("source import `{specifier}` must be package-relative"),
                ));
                continue;
            }
            let dependency_name = dependency_name(specifier);
            if !declared.contains(&dependency_name) {
                findings.insert(ValidationFinding::new(
                    "dependency",
                    format!("source imports undeclared dependency `{dependency_name}`"),
                ));
            }
        }
    }
}

fn parse_typescript(source: &str, path: &Path) -> Option<Tree> {
    let mut parser = Parser::new();
    let language = match path.extension().and_then(|extension| extension.to_str()) {
        Some("tsx" | "jsx") => tree_sitter_typescript::LANGUAGE_TSX,
        _ => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
    };
    parser.set_language(&language.into()).ok()?;
    parser.parse(source, /*old_tree*/ None)
}

fn tree_exports_type(tree: &Tree, source: &str, expected: &str) -> bool {
    top_level_exports(tree).any(|export| {
        if let Some(declaration) = export.child_by_field_name("declaration")
            && matches!(
                declaration.kind(),
                "interface_declaration" | "type_alias_declaration"
            )
            && node_field_text(declaration, "name", source) == Some(expected)
        {
            return true;
        }
        (0..export.named_child_count()).any(|index| {
            let Some(clause) = export.named_child(index) else {
                return false;
            };
            clause.kind() == "export_clause"
                && (0..clause.named_child_count()).any(|index| {
                    let Some(specifier) = clause.named_child(index) else {
                        return false;
                    };
                    specifier.kind() == "export_specifier"
                        && node_field_text(specifier, "alias", source)
                            .or_else(|| node_field_text(specifier, "name", source))
                            == Some(expected)
                        && (export.child_by_field_name("source").is_some()
                            || node_field_text(specifier, "name", source).is_some_and(
                                |local_name| {
                                    tree_has_top_level_type_binding(tree, source, local_name)
                                },
                            ))
                })
        })
    })
}

fn tree_has_top_level_type_binding(tree: &Tree, source: &str, expected: &str) -> bool {
    let root = tree.root_node();
    for index in 0..root.named_child_count() {
        let Some(node) = root.named_child(index) else {
            continue;
        };
        let declaration = if node.kind() == "export_statement" {
            node.child_by_field_name("declaration")
        } else {
            Some(node)
        };
        if declaration.is_some_and(|declaration| {
            matches!(
                declaration.kind(),
                "interface_declaration" | "type_alias_declaration"
            ) && node_field_text(declaration, "name", source) == Some(expected)
        }) {
            return true;
        }
        if node.kind() == "import_statement" && subtree_has_identifier(node, source, expected) {
            return true;
        }
    }
    false
}

fn subtree_has_identifier(root: Node<'_>, source: &str, expected: &str) -> bool {
    let mut nodes = vec![root];
    while let Some(node) = nodes.pop() {
        if matches!(node.kind(), "identifier" | "type_identifier")
            && node_text(node, source) == Some(expected)
        {
            return true;
        }
        for index in 0..node.named_child_count() {
            if let Some(child) = node.named_child(index) {
                nodes.push(child);
            }
        }
    }
    false
}

fn tree_exports_default_define_workflow(tree: &Tree, source: &str) -> bool {
    top_level_exports(tree).any(|export| {
        let Some(value) = export.child_by_field_name("value") else {
            return false;
        };
        value.kind() == "call_expression"
            && node_field_text(value, "function", source) == Some("defineWorkflow")
    })
}

fn top_level_exports(tree: &Tree) -> impl Iterator<Item = Node<'_>> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|node| node.kind() == "export_statement")
        .collect::<Vec<_>>()
        .into_iter()
}

fn tree_has_nonliteral_dependency_call(tree: &Tree, source: &str) -> bool {
    let mut nodes = vec![tree.root_node()];
    while let Some(node) = nodes.pop() {
        if node.kind() == "call_expression"
            && let Some(function) = node.child_by_field_name("function")
        {
            let function_text = node_text(function, source);
            if function.kind() == "import" {
                let arguments = node.child_by_field_name("arguments");
                let has_literal_path = arguments.is_some_and(|arguments| match arguments.kind() {
                    "arguments" => arguments
                        .named_child(/*index*/ 0)
                        .is_some_and(|argument| argument.kind() == "string"),
                    "template_string" => arguments.named_child_count() == 0,
                    _ => false,
                });
                if !has_literal_path {
                    return true;
                }
            } else if function.kind() == "identifier" && function_text == Some("require") {
                return true;
            }
        }
        for index in 0..node.named_child_count() {
            if let Some(child) = node.named_child(index) {
                nodes.push(child);
            }
        }
    }
    false
}

fn node_field_text<'a>(node: Node<'_>, field: &str, source: &'a str) -> Option<&'a str> {
    node.child_by_field_name(field)
        .and_then(|node| node_text(node, source))
}

fn node_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    node.utf8_text(source.as_bytes()).ok()
}

fn validate_local_import(
    package: &WorkflowPackage,
    source_path: &Path,
    specifier: &str,
    findings: &mut BTreeSet<ValidationFinding>,
) {
    let Some(parent) = source_path.parent() else {
        return;
    };
    let unresolved = parent.join(specifier);
    let mut candidates = vec![unresolved.clone()];
    for extension in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
        candidates.push(unresolved.with_extension(extension));
        candidates.push(unresolved.join(format!("index.{extension}")));
    }
    let Some(resolved) = candidates.iter().find(|candidate| candidate.exists()) else {
        findings.insert(ValidationFinding::new(
            "dependency",
            format!(
                "local import `{specifier}` from {} does not resolve",
                source_path.display()
            ),
        ));
        return;
    };
    let Ok(source_root) = package.root.join("src").canonicalize() else {
        return;
    };
    let Ok(resolved) = resolved.canonicalize() else {
        return;
    };
    if !resolved.starts_with(source_root) {
        findings.insert(ValidationFinding::new(
            "dependency",
            format!(
                "local import `{specifier}` from {} resolves outside the workflow src directory",
                source_path.display()
            ),
        ));
    }
}

fn dependency_name(specifier: &str) -> String {
    if specifier.starts_with('@') {
        specifier
            .splitn(/*n*/ 3, '/')
            .take(/*n*/ 2)
            .collect::<Vec<_>>()
            .join("/")
    } else {
        specifier.split('/').next().unwrap_or(specifier).to_string()
    }
}

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
