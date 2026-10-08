#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use pretty_assertions::assert_eq;

use super::*;

const MANIFEST: &str = "apiVersion: 1\nid: team/build\ntitle: Team Build\ncallableName: team-build\ndescription: Build workflow\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run Git");
    assert!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn source(root: &Path, id: &str, tagged: bool) {
    fs::create_dir(root).expect("source root");
    fs::create_dir(root.join("src")).expect("source directory");
    fs::write(
        root.join("workflow.yaml"),
        MANIFEST.replace("team/build", id),
    )
    .expect("manifest");
    let package = if tagged {
        r#"{"version":"1.2.3"}"#
    } else {
        "{}"
    };
    fs::write(root.join("package.json"), package).expect("package");
    fs::write(root.join("src/workflow.ts"), "export default {};\n").expect("workflow source");
    git(root, &["init", "-q"]);
    git(root, &["add", "--all"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "initial",
        ],
    );
    if tagged {
        git(root, &["tag", "v1.2.3"]);
    }
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

#[test]
fn installs_tagged_and_untagged_local_releases_with_prompt_policy() {
    for tagged in [false, true] {
        let root = tempfile::tempdir().expect("root");
        let repository = root.path().join("source");
        source(&repository, "team/build", tagged);
        let service = ManagedWorkflowService::new(
            &absolute(root.path()),
            &absolute(&root.path().join("workflows")),
        )
        .expect("service");
        let source = if tagged {
            repository.to_str().expect("UTF-8 path").to_string()
        } else {
            url::Url::from_file_path(&repository)
                .expect("file URL")
                .to_string()
        };
        let cancelled = AtomicBool::new(false);
        let request = ManagedWorkflowInstallRequest {
            source: &source,
            dependency_runtime: None,
            cancelled: &cancelled,
        };
        let installed = service.install(request).expect("install managed release");
        let receipts = service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog");
        let receipt = &receipts[0];
        assert_eq!(receipt.id, installed.id);
        assert_eq!(
            receipt.installed.version,
            installed.release.version.map(|v| v.to_string())
        );
        assert_eq!(
            receipt.installed.commit,
            installed.release.advertised_object_id
        );
        assert_eq!(receipt.policy, WorkflowUpdatePolicy::Prompt);
    }
}

#[test]
fn refuses_install_inside_an_existing_managed_release() {
    let root = tempfile::tempdir().expect("root");
    let ancestor_source = root.path().join("ancestor-source");
    let child_source = root.path().join("child-source");
    source(&ancestor_source, "team", /*tagged*/ false);
    source(&child_source, "team/build", /*tagged*/ false);
    let service = ManagedWorkflowService::new(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("service");
    let cancelled = AtomicBool::new(false);
    service
        .install(ManagedWorkflowInstallRequest {
            source: ancestor_source.to_str().expect("UTF-8 path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("install ancestor");
    let receipts = service
        .store
        .list_receipts(/*cancelled*/ None)
        .expect("ancestor receipt");
    let error = service
        .install(ManagedWorkflowInstallRequest {
            source: child_source.to_str().expect("UTF-8 path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect_err("child must not alter ancestor payload");
    assert!(error.to_string().contains("inside an installed workflow"));
    assert!(!root.path().join("workflows/team/build").exists());
    assert_eq!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("unchanged receipts"),
        receipts
    );
}

#[test]
fn duplicate_install_keeps_the_published_release() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, "team/build", /*tagged*/ false);
    let service = ManagedWorkflowService::new(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("service");
    let cancelled = AtomicBool::new(false);
    let source = repository.to_str().expect("UTF-8 path");
    let installed = service
        .install(ManagedWorkflowInstallRequest {
            source,
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("initial install");
    let receipts = service
        .store
        .list_receipts(/*cancelled*/ None)
        .expect("initial receipt");
    assert!(
        service
            .install(ManagedWorkflowInstallRequest {
                source,
                dependency_runtime: None,
                cancelled: &cancelled,
            })
            .is_err()
    );
    assert_eq!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("unchanged receipt"),
        receipts
    );
    assert_eq!(receipts[0].source, installed.source);
    assert!(
        root.path()
            .join("workflows/team/build/src/workflow.ts")
            .is_file()
    );
}

#[test]
fn unavailable_remote_source_keeps_the_store_empty() {
    let root = tempfile::tempdir().expect("root");
    let service = ManagedWorkflowService::new(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("service");
    let cancelled = AtomicBool::new(false);
    assert!(
        service
            .install(ManagedWorkflowInstallRequest {
                source: "https://127.0.0.1:1/workflow.git",
                dependency_runtime: None,
                cancelled: &cancelled,
            })
            .is_err()
    );
    assert!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")
            .is_empty()
    );
}

#[test]
fn unmanaged_target_is_never_replaced() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, "team/build", /*tagged*/ false);
    let service = ManagedWorkflowService::new(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("service");
    let target = root.path().join("workflows/team/build");
    fs::create_dir_all(&target).expect("unmanaged target");
    fs::write(target.join("sentinel"), "untouched").expect("unmanaged file");
    let cancelled = AtomicBool::new(false);
    assert!(
        service
            .install(ManagedWorkflowInstallRequest {
                source: repository.to_str().expect("UTF-8 path"),
                dependency_runtime: None,
                cancelled: &cancelled,
            })
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(target.join("sentinel")).expect("unmanaged file"),
        "untouched"
    );
    assert!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")
            .is_empty()
    );
}

fn dependency_source(root: &Path) {
    source(root, "team/build", /*tagged*/ false);
    fs::create_dir_all(root.join("vendor/dep")).expect("local dependency");
    fs::write(
        root.join("package.json"),
        r#"{"name":"team-build","version":"1.0.0","dependencies":{"dep":"file:vendor/dep"}}"#,
    )
    .expect("dependency manifest");
    fs::write(
        root.join("vendor/dep/package.json"),
        r#"{"name":"dep","version":"1.0.0","main":"index.js"}"#,
    )
    .expect("local package");
    fs::write(root.join("vendor/dep/index.js"), "export default 1;\n").expect("local entrypoint");
    fs::write(
        root.join("bun.lock"),
        r#"{"lockfileVersion":1,"configVersion":1,"workspaces":{"":{"name":"team-build","dependencies":{"dep":"file:vendor/dep"}}},"packages":{"dep":["dep@file:vendor/dep",{}]}}"#,
    )
    .expect("text lock");
    git(root, &["add", "--all"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "dependencies",
        ],
    );
}

#[test]
fn dependency_install_requires_runtime_before_publication() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    dependency_source(&repository);
    let service = ManagedWorkflowService::new(
        &absolute(root.path()),
        &absolute(&root.path().join("workflows")),
    )
    .expect("service");
    let cancelled = AtomicBool::new(false);
    let error = service
        .install(ManagedWorkflowInstallRequest {
            source: repository.to_str().expect("UTF-8 path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect_err("Bun runtime is required");
    assert!(
        error
            .to_string()
            .contains("require Bun and a local sandbox")
    );
    assert!(!root.path().join("workflows/team/build").exists());
    assert!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")
            .is_empty()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unavailable_sandbox_preserves_uninstalled_dependency_release() {
    use codex_protocol::config_types::WindowsSandboxLevel;
    use codex_sandboxing::SandboxDirectSpawnRuntime;

    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    dependency_source(&repository);
    let fake_bun = tempfile::tempdir().expect("fake Bun location");
    let bun_path = fake_bun.path().join("bun");
    fs::write(&bun_path, b"not executed").expect("fake Bun file");
    let bun = absolute(&bun_path);
    let home = absolute(root.path());
    let service = ManagedWorkflowService::new(&home, &absolute(&root.path().join("workflows")))
        .expect("service");
    let cancelled = AtomicBool::new(false);
    let error = service
        .install(ManagedWorkflowInstallRequest {
            source: repository.to_str().expect("UTF-8 path"),
            dependency_runtime: Some(ManagedWorkflowDependencyRuntime {
                bun_executable: &bun,
                sandbox: LocalSandboxRuntime {
                    direct_spawn: SandboxDirectSpawnRuntime {
                        codex_home: &home,
                        windows_sandbox_wrapper_executable: None,
                    },
                    linux_sandbox_executable: None,
                    use_legacy_landlock: false,
                    windows_sandbox_level: WindowsSandboxLevel::Disabled,
                    windows_sandbox_private_desktop: false,
                },
            }),
            cancelled: &cancelled,
        })
        .expect_err("sandbox is required");
    assert!(error.to_string().contains("sandbox is unavailable"));
    assert!(!root.path().join("workflows/team/build").exists());
    assert!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")
            .is_empty()
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires Bun and a provisioned Linux sandbox; run in workflow-runtime validation"]
fn sandboxed_local_dependency_install_publishes_verified_tree() {
    use codex_protocol::config_types::WindowsSandboxLevel;
    use codex_sandboxing::SandboxDirectSpawnRuntime;

    let sandbox_helper = std::env::var_os("CODEX_WORKFLOW_TEST_SANDBOX")
        .expect("CODEX_WORKFLOW_TEST_SANDBOX must name the built sandbox helper");
    let bun = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|directory| directory.join("bun"))
        .find(|candidate| candidate.is_file())
        .expect("Bun must be installed for workflow-runtime validation");
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    dependency_source(&repository);
    let home = absolute(root.path());
    let service = ManagedWorkflowService::new(&home, &absolute(&root.path().join("workflows")))
        .expect("service");
    let bun = absolute(&bun);
    let sandbox_helper = absolute(Path::new(&sandbox_helper));
    let cancelled = AtomicBool::new(false);
    let installed = service
        .install(ManagedWorkflowInstallRequest {
            source: repository.to_str().expect("UTF-8 path"),
            dependency_runtime: Some(ManagedWorkflowDependencyRuntime {
                bun_executable: &bun,
                sandbox: LocalSandboxRuntime {
                    direct_spawn: SandboxDirectSpawnRuntime {
                        codex_home: &home,
                        windows_sandbox_wrapper_executable: None,
                    },
                    linux_sandbox_executable: Some(&sandbox_helper),
                    use_legacy_landlock: true,
                    windows_sandbox_level: WindowsSandboxLevel::Disabled,
                    windows_sandbox_private_desktop: false,
                },
            }),
            cancelled: &cancelled,
        })
        .expect("sandboxed dependency install");
    assert!(!installed.cleanup_pending);
    assert_eq!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")[0]
            .id,
        installed.id
    );
    assert!(
        root.path()
            .join("workflows/team/build/node_modules/dep")
            .exists()
    );
}
