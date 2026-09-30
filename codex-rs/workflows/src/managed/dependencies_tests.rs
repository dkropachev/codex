use std::fs;
use std::path::Path;
use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;

struct Package(tempfile::TempDir, crate::WorkflowPackage);

impl Package {
    fn new(package_json: serde_json::Value) -> Self {
        let temporary = tempfile::tempdir().expect("temporary package");
        fs::create_dir_all(temporary.path().join("src")).expect("create source directory");
        fs::write(
            temporary.path().join("workflow.yaml"),
            "apiVersion: 1\nid: test/workflow\ntitle: Test workflow\ncallableName: test-workflow\ndescription: Test package\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
        )
        .expect("write manifest");
        write_json(temporary.path().join("package.json"), &package_json);
        fs::write(
            temporary.path().join("src/workflow.ts"),
            "export default {};\n",
        )
        .expect("write source");
        let package = crate::WorkflowPackage::load(temporary.path()).expect("load package");
        Self(temporary, package)
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn validate(&self) -> anyhow::Result<ValidatedDependencySources> {
        validate_managed_dependency_sources(&self.1)
    }
}

#[test]
fn accepts_public_registry_grammar_and_nested_overrides() {
    let empty = Package::new(serde_json::json!({"name": "test"}));
    assert_eq!(
        empty.validate().expect("dependency-free package"),
        ValidatedDependencySources {
            has_dependencies: false,
            local_packages: Vec::new(),
        }
    );
    let package = Package::new(serde_json::json!({
        "dependencies": {"plain": "latest", "range": "^1.2.3 || ~2.0.0"},
        "devDependencies": {"alias": "npm:@scope/package@1.2.x"},
        "optionalDependencies": {"optional": "*"},
        "peerDependencies": {"peer": ">=1.0.0 <3"},
        "overrides": {"plain": {".": "2.0.0", "child": "~1.0.0"}},
        "resolutions": {"peer": "1.5.0"}
    }));
    assert_eq!(
        package.validate().expect("valid public dependencies"),
        ValidatedDependencySources {
            has_dependencies: true,
            local_packages: Vec::new(),
        }
    );
}

#[test]
fn recursively_validates_contained_local_packages() {
    let package = Package::new(serde_json::json!({
        "dependencies": {"a": "file:vendor/a"}
    }));
    for path in ["vendor/a", "vendor/b"] {
        fs::create_dir_all(package.root().join(path)).expect("create local package");
    }
    write_json(
        package.root().join("vendor/a/package.json"),
        &serde_json::json!({"dependencies": {"b": "file:../b", "public": "1.0.0"}}),
    );
    write_json(
        package.root().join("vendor/b/package.json"),
        &serde_json::json!({"dependencies": {"a": "file:../a"}}),
    );
    assert_eq!(
        package.validate().expect("valid local dependency graph"),
        ValidatedDependencySources {
            has_dependencies: true,
            local_packages: vec![PathBuf::from("vendor/a"), PathBuf::from("vendor/b")],
        }
    );
}

#[test]
fn deduplicates_local_edges_and_allows_exact_package_limit() {
    let mut dependencies = serde_json::Map::new();
    for index in 0..MAX_LOCAL_PACKAGES {
        dependencies.insert(
            format!("local-{index}"),
            serde_json::Value::String(format!("file:vendor/p{index}")),
        );
    }
    dependencies.insert(
        "duplicate".to_string(),
        serde_json::Value::String("file:vendor/p0".to_string()),
    );
    let mut package = Package::new(serde_json::json!({"dependencies": dependencies}));
    for index in 0..MAX_LOCAL_PACKAGES {
        let directory = package.root().join(format!("vendor/p{index}"));
        fs::create_dir_all(&directory).expect("create local package");
        write_json(directory.join("package.json"), &serde_json::json!({}));
    }
    let validated = package.validate().expect("exact local package limit");
    assert_eq!(validated.local_packages.len(), MAX_LOCAL_PACKAGES);

    let extra = package.root().join("vendor/extra");
    fs::create_dir_all(&extra).expect("create extra package");
    write_json(extra.join("package.json"), &serde_json::json!({}));
    package.1.package_json["dependencies"]["too-many"] =
        serde_json::Value::String("file:vendor/extra".to_string());
    assert!(
        package
            .validate()
            .unwrap_err()
            .to_string()
            .contains("exceeds 128")
    );
}

#[test]
fn rejects_non_registry_and_unsafe_local_specifiers() {
    for value in "|.|café|bad<name|bad\nname|C:/path|%2e%2e".split('|') {
        assert!(resolve_local_path(Path::new("vendor"), value).is_err());
    }
    assert!(resolve_local_path(Path::new("vendor"), &"a".repeat(256)).is_err());
    for specifier in [
        "../outside",
        "./outside",
        "file:../outside",
        "file:vendor/missing",
        "link:vendor/a",
        "workspace:*",
        "catalog:default",
        "github:owner/repo",
        "git+https://example.com/repo.git",
        "https://example.com/archive.tgz",
        "$other",
        "npm:@scope/package",
        "npm:bad:name@1.0.0",
    ] {
        let package = Package::new(serde_json::json!({"dependencies": {"bad": specifier}}));
        assert!(package.validate().is_err(), "accepted {specifier:?}");
    }

    for name in [
        "../bad",
        "bad name",
        "@scope",
        "@scope/name/extra",
        "bad:name",
    ] {
        let package = Package::new(serde_json::json!({"dependencies": {(name): "1.0.0"}}));
        assert!(package.validate().is_err(), "accepted name {name:?}");
    }
}

#[test]
fn rejects_unsupported_features_and_nested_local_escape() {
    for field in ["workspaces", "catalog", "catalogs", "patchedDependencies"] {
        let package = Package::new(serde_json::json!({(field): {"x": "1.0.0"}}));
        assert!(package.validate().unwrap_err().to_string().contains(field));
    }
    let package = Package::new(serde_json::json!({"dependencies": {"local": "file:vendor/a"}}));
    fs::create_dir_all(package.root().join("vendor/a")).expect("create local package");
    write_json(
        package.root().join("vendor/a/package.json"),
        &serde_json::json!({"dependencies": {"escape": "file:../../../outside"}}),
    );
    assert!(
        package
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unsafe path")
    );

    for nested in [
        serde_json::json!({"dependencies": {"bad": "git+https://example.com/repo"}}),
        serde_json::json!({"patchedDependencies": {"bad": "patches/bad.patch"}}),
        serde_json::json!({"overrides": {"bad": "https://example.com/archive.tgz"}}),
    ] {
        let package = Package::new(serde_json::json!({"dependencies": {"local": "file:vendor/a"}}));
        fs::create_dir_all(package.root().join("vendor/a")).expect("create local package");
        write_json(package.root().join("vendor/a/package.json"), &nested);
        assert!(
            package.validate().is_err(),
            "accepted nested manifest {nested}"
        );
    }
}

#[test]
fn rejects_invalid_local_package_targets_and_manifests() {
    for contents in [None, Some("{"), Some("[]")] {
        let package =
            Package::new(serde_json::json!({"dependencies": {"local": "file:vendor/local"}}));
        fs::create_dir_all(package.root().join("vendor/local")).expect("create local package");
        if let Some(contents) = contents {
            fs::write(package.root().join("vendor/local/package.json"), contents)
                .expect("write local manifest");
        }
        assert!(package.validate().is_err());
    }
    let package = Package::new(serde_json::json!({"dependencies": {"local": "file:vendor/local"}}));
    fs::create_dir(package.root().join("vendor")).expect("create vendor");
    fs::write(package.root().join("vendor/local"), "not a directory").expect("write local target");
    assert!(package.validate().is_err());
}

#[test]
fn rejects_malformed_dependency_and_override_shapes() {
    for package_json in [
        serde_json::json!({"dependencies": []}),
        serde_json::json!({"devDependencies": {"bad": 1}}),
        serde_json::json!({"optionalDependencies": null}),
        serde_json::json!({"overrides": {"bad": 1}}),
        serde_json::json!({"resolutions": ["1.0.0"]}),
    ] {
        let package = Package::new(package_json.clone());
        assert!(
            package.validate().is_err(),
            "accepted malformed manifest {package_json}"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_local_package_or_manifest() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().expect("outside directory");
    write_json(
        outside.path().join("package.json"),
        &serde_json::json!({"name": "outside"}),
    );
    let package = Package::new(serde_json::json!({"dependencies": {"local": "file:vendor/local"}}));
    fs::create_dir(package.root().join("vendor")).expect("create vendor");
    symlink(outside.path(), package.root().join("vendor/local")).expect("link local package");
    assert!(package.validate().is_err());

    fs::remove_file(package.root().join("vendor/local")).expect("remove directory link");
    fs::create_dir(package.root().join("vendor/local")).expect("create local package");
    symlink(
        outside.path().join("package.json"),
        package.root().join("vendor/local/package.json"),
    )
    .expect("link local manifest");
    assert!(package.validate().is_err());
}

fn write_json(path: impl AsRef<Path>, value: &serde_json::Value) {
    fs::write(
        path,
        serde_json::to_vec(value).expect("serialize package manifest"),
    )
    .expect("write package manifest");
}
