use std::collections::BTreeSet;
use std::fs;

use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::ScaffoldRequest;
use crate::WorkflowPackage;
use crate::scaffold_workflow;

#[test]
fn findings_are_deterministic_for_missing_and_legacy_packages() {
    let temp = TempDir::new().expect("tempdir");
    fs::write(
        temp.path().join("workflow.yaml"),
        "id: old\ncommand: old\nuserDescription: old package\n",
    )
    .expect("write legacy metadata");

    let first = validate_workflow(temp.path());
    let second = validate_workflow(temp.path());
    assert_eq!(first, second);
    assert!(!first.is_valid());
    assert!(first.render().contains("legacy workflow metadata field"));
    assert!(first.render().contains("required package file"));
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn reports_undeclared_and_non_local_dependencies() {
    let (_registry, root) = scaffold();
    let mut package_json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join("package.json")).expect("read package.json"),
    )
    .expect("parse package.json");
    package_json["dependencies"] = json!({ "outside": "^1.0.0" });
    fs::write(
        root.join("package.json"),
        serde_json::to_string_pretty(&package_json).expect("serialize package.json"),
    )
    .expect("write package.json");
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read source");
    fs::write(
        source_path,
        format!("await import(\"fs\");\nimport \"missing\";\n{source}"),
    )
    .expect("write source");

    let rendered = validate_workflow(&root).render();
    assert!(
        rendered.contains("dependency `outside` is declared but not installed"),
        "{rendered}"
    );
    assert!(
        rendered.contains("source imports undeclared dependency `missing`"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("undeclared dependency `fs`"),
        "{rendered}"
    );
}

#[test]
fn reports_layout_gitignore_exports_coverage_and_command_failures() {
    let (_registry, root) = scaffold();
    fs::remove_file(root.join("DESIGN.md")).expect("remove design");
    fs::write(root.join(".gitignore"), "state/*\n").expect("write gitignore");
    fs::write(
        root.join("src/tests/workflow.test.ts"),
        "// workflow-covers: positive\n",
    )
    .expect("replace coverage markers");
    fs::write(
        root.join("src/workflow.ts"),
        "export interface WorkflowInput {}\n",
    )
    .expect("replace source");
    let mut package = WorkflowPackage::load(&root).expect("load package");
    package.manifest.validation.commands = vec![crate::ValidationCommand {
        program: "rustc".to_string(),
        args: vec!["--definitely-not-a-real-rustc-option".to_string()],
    }];
    fs::write(
        root.join("workflow.yaml"),
        serde_yaml::to_string(&package.manifest).expect("serialize manifest"),
    )
    .expect("write manifest");

    let rendered = validate_workflow(&root).render();
    for expected in [
        "missing or non-regular required package file `DESIGN.md`",
        ".gitignore is missing `!state/.gitkeep`",
        "missing the `WorkflowOutput` export",
        "workflow-covers: load",
        "workflow-covers: autocomplete",
        "workflow-covers: negative",
        "validation command 0 `rustc` with args",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected:?} in {rendered}"
        );
    }
}

#[test]
fn validation_commands_treat_shell_metacharacters_as_literal_arguments() {
    let (_registry, root) = scaffold();
    let side_effect = root.join("shell-side-effect");
    let literal_argument = if cfg!(windows) {
        format!("& type nul > \"{}\"", side_effect.display())
    } else {
        format!("; touch '{}'", side_effect.display())
    };
    let mut package = WorkflowPackage::load(&root).expect("load package");
    package.manifest.validation.commands = vec![crate::ValidationCommand {
        program: "git".to_string(),
        args: vec!["not-a-command".to_string(), literal_argument.clone()],
    }];

    let mut findings = BTreeSet::new();
    super::checks::validate_commands(&package, &mut findings);

    assert!(!side_effect.exists());
    assert!(
        findings
            .iter()
            .any(|finding| finding.message.contains(&literal_argument)),
        "expected the literal argument in {findings:?}"
    );
}

#[test]
fn syntax_tree_distinguishes_exports_and_nonliteral_dependency_calls() {
    let source = r#"
// export interface WorkflowInput {}
/* export default defineWorkflow({}); */
const fake = "export interface WorkflowOutput {}";
import {
  unsafe
} from "/outside.ts";
const lazy = import(variable);
const matcher = /import\s*\(/;
loader.import(variable);
loader.require(variable);
const ratio = counter++ / import(target);
const asserted = value! / import(otherTarget);
const templateAsserted = `value`! / import(templateTarget);
const regexAsserted = /value/! / import(regexTarget);
const jsxRatio = <div /> / import(jsxTarget);
"#;
    let tree = parse_typescript(source, Path::new("source.tsx")).expect("parse TSX");
    assert!(!tree_exports_type(&tree, source, "WorkflowInput"));
    assert!(!tree_exports_type(&tree, source, "WorkflowOutput"));
    assert!(!tree_exports_default_define_workflow(&tree, source));
    assert!(tree_has_nonliteral_dependency_call(&tree, source));

    let benign = r#"
const matcher = () => /require()/.test(value);
loader.import(variable);
loader.require(variable);
await import("node:fs");
"#;
    let tree = parse_typescript(benign, Path::new("source.ts")).expect("parse TypeScript");
    assert!(!tree_has_nonliteral_dependency_call(&tree, benign));

    let exported = "interface InternalInput {}\nexport type { InternalInput as WorkflowInput };";
    let tree = parse_typescript(exported, Path::new("source.ts")).expect("parse export list");
    assert!(tree_exports_type(&tree, exported, "WorkflowInput"));
    let renamed = "interface WorkflowInput {}\nexport type { WorkflowInput as InternalInput };";
    let tree = parse_typescript(renamed, Path::new("source.ts")).expect("parse renamed export");
    assert!(!tree_exports_type(&tree, renamed, "WorkflowInput"));
    let missing = "export type { Missing as WorkflowInput };";
    let tree = parse_typescript(missing, Path::new("source.ts")).expect("parse missing binding");
    assert!(!tree_exports_type(&tree, missing, "WorkflowInput"));
    let nested = "export namespace Hidden {\n  export interface WorkflowInput {}\n}\n";
    let tree = parse_typescript(nested, Path::new("source.ts")).expect("parse namespace");
    assert!(!tree_exports_type(&tree, nested, "WorkflowInput"));
    let runtime_reexport = "export { value as WorkflowInput } from './helper';";
    let tree = parse_typescript(runtime_reexport, Path::new("source.ts"))
        .expect("parse runtime re-export");
    assert!(!tree_exports_type(&tree, runtime_reexport, "WorkflowInput"));
    let type_reexport = "export type { Value as WorkflowInput } from './helper';";
    let tree =
        parse_typescript(type_reexport, Path::new("source.ts")).expect("parse type re-export");
    assert!(tree_exports_type(&tree, type_reexport, "WorkflowInput"));
    let runtime_import =
        "import { Value as WorkflowInput } from './helper'; export { WorkflowInput };";
    let tree =
        parse_typescript(runtime_import, Path::new("source.ts")).expect("parse runtime import");
    assert!(!tree_exports_type(&tree, runtime_import, "WorkflowInput"));
    let type_import =
        "import type { Value as WorkflowInput } from './helper'; export { WorkflowInput };";
    let tree = parse_typescript(type_import, Path::new("source.ts")).expect("parse type import");
    assert!(tree_exports_type(&tree, type_import, "WorkflowInput"));
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_required_directories_and_installed_dependencies() {
    use std::os::unix::fs::symlink;

    let (registry, root) = scaffold();
    let outside_tests = registry.path().join("outside-tests");
    fs::create_dir(&outside_tests).expect("create outside tests");
    fs::remove_dir_all(root.join("src/tests")).expect("remove tests directory");
    symlink(&outside_tests, root.join("src/tests")).expect("link tests directory");
    let mut findings = BTreeSet::new();
    validate_required_layout(&root, &mut findings);
    assert!(
        findings
            .iter()
            .any(|finding| finding.message.contains("src/tests"))
    );

    let outside_dependency = registry.path().join("outside-dependency");
    fs::create_dir(&outside_dependency).expect("create outside dependency");
    fs::create_dir(root.join("node_modules")).expect("create node_modules");
    symlink(&outside_dependency, root.join("node_modules/escape")).expect("link dependency");
    let mut package = WorkflowPackage::load(&root).expect("load package");
    package.package_json["dependencies"] = json!({ "escape": "1.0.0" });
    let mut findings = BTreeSet::new();
    validate_package_json(&package, &mut findings);
    assert!(
        findings
            .iter()
            .any(|finding| finding.message.contains("not installed"))
    );
}

#[cfg(unix)]
#[test]
fn rejects_an_intermediate_source_symlink_and_skips_nested_test_symlinks() {
    use std::os::unix::fs::symlink;

    let (registry, root) = scaffold();
    let outside_source = registry.path().join("outside-source");
    fs::create_dir(&outside_source).expect("create outside source");
    fs::copy(
        root.join("src/workflow.ts"),
        outside_source.join("workflow.ts"),
    )
    .expect("copy workflow source");
    fs::remove_file(root.join("src/workflow.ts")).expect("remove source");
    fs::remove_dir_all(root.join("src/tests")).expect("remove tests");
    fs::remove_dir(root.join("src")).expect("remove source directory");
    symlink(&outside_source, root.join("src")).expect("link source directory");

    let error = WorkflowPackage::load(&root).expect_err("source symlink must be rejected");
    assert!(format!("{error:#}").contains("regular file inside the package"));
    assert!(
        validate_workflow(&root)
            .render()
            .contains("src/workflow.ts")
    );

    fs::remove_file(root.join("src")).expect("remove source link");
    fs::create_dir_all(root.join("src/tests")).expect("restore tests");
    fs::copy(
        outside_source.join("workflow.ts"),
        root.join("src/workflow.ts"),
    )
    .expect("restore workflow source");
    fs::write(
        root.join("src/tests/workflow.test.ts"),
        "// workflow-covers: positive load autocomplete negative\n",
    )
    .expect("write coverage markers");
    symlink(".", root.join("src/tests/loop")).expect("create test loop");
    let package = WorkflowPackage::load(&root).expect("load restored package");
    let mut findings = BTreeSet::new();
    super::checks::validate_coverage(&package, &mut findings);
    assert_eq!(findings, BTreeSet::new());
}

#[test]
fn coverage_scan_is_bounded() {
    let (_registry, root) = scaffold();
    for index in 0..=256 {
        fs::write(
            root.join("src/tests").join(format!("coverage-{index}.ts")),
            "// no marker\n",
        )
        .expect("write coverage file");
    }
    let package = WorkflowPackage::load(&root).expect("load package");
    let mut findings = BTreeSet::new();
    super::checks::validate_coverage(&package, &mut findings);
    assert!(findings.iter().any(|finding| {
        finding
            .message
            .contains("coverage scan exceeded its file, byte, entry, or depth limit")
    }));
}

#[test]
fn metadata_reads_are_bounded() {
    let (_registry, root) = scaffold();
    fs::write(
        root.join("workflow.yaml"),
        "#".repeat((crate::manifest::MAX_WORKFLOW_YAML_BYTES + 1) as usize),
    )
    .expect("write oversized workflow metadata");
    let rendered = validate_workflow(&root).render();
    assert!(rendered.contains("65536-byte limit"), "{rendered}");

    let (_registry, root) = scaffold();
    fs::write(
        root.join("package.json"),
        " ".repeat((crate::manifest::MAX_PACKAGE_JSON_BYTES + 1) as usize),
    )
    .expect("write oversized package metadata");
    let rendered = validate_workflow(&root).render();
    assert!(rendered.contains("1048576-byte limit"), "{rendered}");
}

#[test]
fn tracked_artifacts_are_rejected() {
    let (_registry, root) = scaffold();
    fs::create_dir(root.join("artifacts")).expect("create artifacts");
    fs::write(root.join("artifacts/result.json"), "{}\n").expect("write artifact");
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["add", "-f", "artifacts/result.json"])
        .status()
        .expect("run git add");
    assert!(status.success());
    let mut findings = BTreeSet::new();
    super::checks::validate_git_layout(&root, &mut findings);
    assert!(
        findings
            .iter()
            .any(|finding| finding.message.contains("artifacts/result.json"))
    );
}

#[test]
fn invalid_git_layout_and_unreadable_gitignore_are_rejected() {
    let (_registry, root) = scaffold();
    fs::remove_dir_all(root.join(".git")).expect("remove git repository");
    fs::create_dir(root.join(".git")).expect("create invalid git directory");
    fs::write(
        root.join(".gitignore"),
        "x".repeat((super::checks::MAX_GITIGNORE_BYTES + 1) as usize),
    )
    .expect("write oversized gitignore");

    let mut findings = BTreeSet::new();
    super::checks::validate_gitignore(&root, &mut findings);
    super::checks::validate_git_layout(&root, &mut findings);

    assert!(findings.iter().any(|finding| {
        finding.code == "gitignore" && finding.message.contains("65536-byte limit")
    }));
    assert!(findings.iter().any(|finding| {
        finding.code == "layout"
            && finding
                .message
                .contains("failed to inspect workflow git repository")
    }));
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn validation_reports_yaml_module_metadata_mismatch() {
    let (_registry, root) = scaffold();
    let yaml_path = root.join("workflow.yaml");
    let yaml = fs::read_to_string(&yaml_path).expect("read workflow metadata");
    fs::write(
        &yaml_path,
        yaml.replace("title: Validate", "title: Different"),
    )
    .expect("write mismatched metadata");

    let rendered = validate_workflow(&root).render();
    assert!(
        rendered.contains("does not match workflow.yaml title"),
        "{rendered}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn validation_detects_multiline_absolute_and_non_literal_imports() {
    let (registry, root) = scaffold();
    let outside = registry.path().join("outside.ts");
    fs::write(&outside, "export const unsafe = 1;\n").expect("write outside module");
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read workflow source");
    let outside = serde_json::to_string(&outside.to_string_lossy()).expect("quote outside path");
    fs::write(
        source_path,
        format!(
            "import {{\n  unsafe\n}} from {outside};\nasync function lazy(name: string) {{ return import(name); }}\nvoid unsafe;\n{source}"
        ),
    )
    .expect("write workflow source");

    let rendered = validate_workflow(&root).render();
    assert!(rendered.contains("must be package-relative"), "{rendered}");
    assert!(
        rendered.contains("non-literal dynamic import or require"),
        "{rendered}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn validation_rejects_local_modules_outside_src() {
    let (_registry, root) = scaffold();
    fs::write(
        root.join("helper.ts"),
        "import 'undeclared';\nexport const helper = true;\n",
    )
    .expect("write package-root helper");
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read workflow source");
    fs::write(
        &source_path,
        format!("import {{ helper }} from '../helper.ts';\nvoid helper;\n{source}"),
    )
    .expect("import package-root helper");

    let rendered = validate_workflow(&root).render();
    assert!(
        rendered.contains("resolves outside the workflow src directory"),
        "{rendered}"
    );
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn validation_accepts_named_export_lists() {
    let (_registry, root) = scaffold();
    let source_path = root.join("src/workflow.ts");
    let source = fs::read_to_string(&source_path).expect("read workflow source");
    let source = source
        .replace("export interface WorkflowInput", "interface WorkflowInput")
        .replace(
            "export interface WorkflowOutput",
            "interface WorkflowOutput",
        )
        .replace("export const inputSchema", "const inputSchema")
        .replace("export const outputSchema", "const outputSchema");
    fs::write(
        &source_path,
        format!(
            "{source}\nexport type {{ WorkflowInput, WorkflowOutput }};\nexport {{ inputSchema, outputSchema }};\n"
        ),
    )
    .expect("write export-list workflow source");

    assert_eq!(validate_workflow(&root), ValidationReport::default());
}

#[test]
#[ignore = "requires Bun; run explicitly in workflow-runtime validation"]
fn fresh_scaffold_validates_with_real_bun() {
    let (_registry, root) = scaffold();

    assert_eq!(validate_workflow(&root), ValidationReport::default());
}

fn scaffold() -> (TempDir, std::path::PathBuf) {
    let registry = TempDir::new().expect("tempdir");
    let root = scaffold_workflow(
        registry.path(),
        &ScaffoldRequest {
            id: "validate".to_string(),
            title: "Validate".to_string(),
            callable_name: "validate".to_string(),
            description: "Validate a package.".to_string(),
        },
    )
    .expect("scaffold workflow");
    (registry, root)
}
