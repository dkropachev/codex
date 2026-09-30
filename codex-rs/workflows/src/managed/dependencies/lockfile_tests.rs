use std::fs;
use std::path::Path;
use std::path::PathBuf;

use pretty_assertions::assert_eq;

use super::*;

struct Package(tempfile::TempDir, crate::WorkflowPackage);

impl Package {
    fn new(package_json: serde_json::Value) -> Self {
        let temporary = tempfile::tempdir().expect("temporary package");
        fs::create_dir(temporary.path().join("src")).expect("create source directory");
        fs::write(
            temporary.path().join("workflow.yaml"),
            "apiVersion: 1\nid: test/workflow\ntitle: Test workflow\ncallableName: test-workflow\ndescription: Test package\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
        )
        .expect("write workflow manifest");
        fs::write(
            temporary.path().join("package.json"),
            serde_json::to_vec(&package_json).expect("serialize package.json"),
        )
        .expect("write package.json");
        fs::write(
            temporary.path().join("src/workflow.ts"),
            "export default {};\n",
        )
        .expect("write workflow source");
        let package = crate::WorkflowPackage::load(temporary.path()).expect("load package");
        Self(temporary, package)
    }

    fn write_lock(&self, contents: impl AsRef<[u8]>) {
        fs::write(self.0.path().join("bun.lock"), contents).expect("write bun.lock");
    }

    fn write_local_manifest(&self, path: &str, value: serde_json::Value) {
        let directory = self.0.path().join(path);
        fs::create_dir_all(&directory).expect("create local package");
        fs::write(
            directory.join("package.json"),
            serde_json::to_vec(&value).expect("serialize local manifest"),
        )
        .expect("write local manifest");
    }
}

fn sources(local: &[&str]) -> ValidatedDependencySources {
    ValidatedDependencySources {
        has_dependencies: true,
        local_packages: local.iter().map(PathBuf::from).collect(),
    }
}

fn registry_lock(specifier: &str, resolution: &str, source: &str) -> serde_json::Value {
    serde_json::json!({
        "lockfileVersion": 1,
        "configVersion": 1,
        "workspaces": {"": {"dependencies": {"dep": specifier}}},
        "packages": {"dep": [resolution, source, {}, ""]}
    })
}

#[test]
fn classifies_dependency_free_text_and_binary_packages() {
    let empty = Package::new(serde_json::json!({}));
    assert_eq!(
        validate(
            &empty.1,
            &ValidatedDependencySources {
                has_dependencies: false,
                local_packages: Vec::new(),
            }
        )
        .expect("dependency-free package"),
        ManagedBunLockfile::NotRequired
    );

    let package = Package::new(serde_json::json!({"dependencies": {"dep": "1.2.3"}}));
    assert!(validate(&package.1, &sources(&[])).is_err());
    fs::write(package.0.path().join("bun.lockb"), [0, 1, 2]).expect("write binary lock");
    assert_eq!(
        validate(&package.1, &sources(&[])).expect("binary lock classification"),
        ManagedBunLockfile::BinaryRequiresSandboxInspection
    );
    package.write_lock("{}");
    assert!(validate(&package.1, &sources(&[])).is_err());
}

#[test]
fn accepts_jsonc_registry_alias_and_optional_peer_metadata() {
    let package = Package::new(serde_json::json!({
        "dependencies": {"dep": "npm:@scope/pkg@^1.0.0"},
        "devDependencies": {},
        "peerDependencies": {"peer": "^2.0.0"},
        "resolutions": {"dep": "1.2.3"}
    }));
    package.write_lock(
        r#"{
          // Generated Bun lockfile.
          "lockfileVersion": 2,
          "configVersion": 1,
          "workspaces": {"": {
            "dependencies": {"dep": "npm:@scope/pkg@^1.0.0",},
            "peerDependencies": {"peer": "^2.0.0"},
            "optionalPeers": ["peer"],
          }},
          "overrides": {"dep": "1.2.3"},
          "packages": {
            "dep": ["dep@npm:@scope/pkg@1.2.3", "", {"os": ["linux"]}, ""],
          },
        }"#,
    );
    assert_eq!(
        validate(&package.1, &sources(&[])).expect("safe text lock"),
        ManagedBunLockfile::TextSourcesValidated
    );
}

#[test]
fn accepts_root_normalized_local_resolutions_and_relative_metadata() {
    let package = Package::new(serde_json::json!({
        "dependencies": {"a": "file:vendor/a"}
    }));
    package.write_local_manifest(
        "vendor/a",
        serde_json::json!({"dependencies": {"b": "file:../b"}}),
    );
    package.write_local_manifest("vendor/b", serde_json::json!({}));
    package.write_lock(
        serde_json::to_vec(&serde_json::json!({
            "lockfileVersion": 1,
            "workspaces": {"": {"dependencies": {"a": "file:vendor/a"}}},
            "packages": {
                "a": ["a@file:vendor/a", {"dependencies": {"b": "file:../b"}}],
                "alias": ["a@file:vendor/a", {"dependencies": {"b": "file:../b"}}],
                "b": ["b@file:vendor/b", {}]
            }
        }))
        .expect("serialize lock"),
    );
    assert_eq!(
        validate(&package.1, &sources(&["vendor/a", "vendor/b"])).expect("local lock"),
        ManagedBunLockfile::TextSourcesValidated
    );

    for (section, dependencies) in [
        ("dependencies", serde_json::json!({"b": "2.0.0"})),
        ("devDependencies", serde_json::json!({"dev": "1.0.0"})),
        (
            "optionalDependencies",
            serde_json::json!({"optional": "1.0.0"}),
        ),
        ("peerDependencies", serde_json::json!({"peer": "1.0.0"})),
    ] {
        let mut manifest = serde_json::json!({"dependencies": {"b": "file:../b"}});
        manifest[section] = dependencies;
        package.write_local_manifest("vendor/a", manifest);
        assert!(
            validate(&package.1, &sources(&["vendor/a", "vendor/b"])).is_err(),
            "accepted stale local {section}"
        );
    }
}

#[test]
fn rejects_unsafe_registry_tuple_sources_and_resolutions() {
    for lock in [
        registry_lock("1.2.3", "dep@1.2.3", "https://registry.example/dep.tgz"),
        registry_lock("1.2.3", "dep@https://example/dep.tgz", ""),
        registry_lock("1.2.3", "dep@github:owner/repo", ""),
        registry_lock("1.2.3", "dep@^1.2.3", ""),
        serde_json::json!({
            "lockfileVersion": 1,
            "workspaces": {"": {"dependencies": {"dep": "1.2.3"}}},
            "packages": {"dep": ["dep@1.2.3", "", {"dependencies": {"x": "file:x"}}, ""]}
        }),
    ] {
        let package = Package::new(serde_json::json!({"dependencies": {"dep": "1.2.3"}}));
        package.write_lock(serde_json::to_vec(&lock).expect("serialize lock"));
        assert!(
            validate(&package.1, &sources(&[])).is_err(),
            "accepted {lock}"
        );
    }
}

#[test]
fn rejects_manifest_mismatch_local_mismatch_duplicates_and_malformed_jsonc() {
    let package = Package::new(serde_json::json!({"dependencies": {"dep": "1.2.3"}}));
    for contents in [
        serde_json::to_string(&registry_lock("2.0.0", "dep@2.0.0", ""))
            .expect("serialize mismatch"),
        r#"{"lockfileVersion":1,"lockfileVersion":1,"workspaces":{"":{"dependencies":{"dep":"1.2.3"}}},"packages":{}}"#.to_string(),
        "{/* unterminated".to_string(),
        serde_json::json!({
            "lockfileVersion": 3,
            "workspaces": {"": {"dependencies": {"dep": "1.2.3"}}},
            "packages": {"dep": ["dep@1.2.3", "", {}, ""]}
        })
        .to_string(),
        serde_json::json!({
            "lockfileVersion": 1,
            "trustedDependencies": ["dep"],
            "workspaces": {"": {"dependencies": {"dep": "1.2.3"}}},
            "packages": {"dep": ["dep@1.2.3", "", {}, ""]}
        })
        .to_string(),
        serde_json::json!({
            "lockfileVersion": 1,
            "overrides": {"dep": "2.0.0"},
            "workspaces": {"": {"dependencies": {"dep": "1.2.3"}}},
            "packages": {"dep": ["dep@1.2.3", "", {}, ""]}
        })
        .to_string(),
    ] {
        package.write_lock(contents);
        assert!(validate(&package.1, &sources(&[])).is_err());
    }

    let local = Package::new(serde_json::json!({"dependencies": {"a": "file:vendor/a"}}));
    local.write_local_manifest("vendor/a", serde_json::json!({}));
    local.write_local_manifest("vendor/missing", serde_json::json!({}));
    local.write_lock(
        serde_json::to_vec(&serde_json::json!({
            "lockfileVersion": 1,
            "workspaces": {"": {"dependencies": {"a": "file:vendor/a"}}},
            "packages": {"a": ["a@file:vendor/a", {}]}
        }))
        .expect("serialize local lock"),
    );
    assert!(validate(&local.1, &sources(&["vendor/a", "vendor/missing"])).is_err());
}

#[test]
fn rejects_oversized_or_non_regular_locks() {
    let package = Package::new(serde_json::json!({"dependencies": {"dep": "1.2.3"}}));
    fs::create_dir(package.0.path().join("bun.lock")).expect("create lock directory");
    assert!(validate(&package.1, &sources(&[])).is_err());
    fs::remove_dir(package.0.path().join("bun.lock")).expect("remove lock directory");
    let binary = fs::File::create(package.0.path().join("bun.lockb")).expect("create binary lock");
    binary
        .set_len(MAX_BUN_LOCK_BYTES + 1)
        .expect("extend binary lock");
    assert!(validate(&package.1, &sources(&[])).is_err());
    fs::remove_file(package.0.path().join("bun.lockb")).expect("remove binary lock");
    fs::write(
        package.0.path().join("bun.lock"),
        vec![b' '; (MAX_BUN_LOCK_BYTES + 1) as usize],
    )
    .expect("write oversized text lock");
    assert!(validate(&package.1, &sources(&[])).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_lock() {
    use std::os::unix::fs::symlink;

    let package = Package::new(serde_json::json!({"dependencies": {"dep": "1.2.3"}}));
    symlink(Path::new("package.json"), package.0.path().join("bun.lock"))
        .expect("create lock symlink");
    assert!(validate(&package.1, &sources(&[])).is_err());
}
