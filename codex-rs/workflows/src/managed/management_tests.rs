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

fn source(root: &Path, tagged: bool, dependencies: bool) {
    fs::create_dir(root).expect("source root");
    fs::create_dir(root.join("src")).expect("source directory");
    fs::write(root.join("workflow.yaml"), MANIFEST).expect("manifest");
    let package = match (tagged, dependencies) {
        (true, false) => r#"{"version":"1.2.3"}"#,
        (true, true) => r#"{"version":"1.2.3","dependencies":{"dep":"1.0.0"}}"#,
        (false, false) => "{}",
        (false, true) => r#"{"dependencies":{"dep":"1.0.0"}}"#,
    };
    fs::write(root.join("package.json"), package).expect("package");
    fs::write(root.join("src/workflow.ts"), "export default {};\n").expect("workflow source");
    if dependencies {
        fs::write(
            root.join("bun.lock"),
            r#"{"lockfileVersion":1,"workspaces":{"":{"dependencies":{"dep":"1.0.0"}}},"packages":{"dep":["dep@1.0.0","",{},"integrity"]}}"#,
        )
        .expect("Bun lock");
    }
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
        source(&repository, tagged, /*dependencies*/ false);
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
        assert_eq!(receipts.len(), 1);
        let receipt = &receipts[0];
        assert_eq!(receipt.id, installed.id);
        assert_eq!(receipt.source, installed.source);
        assert_eq!(receipt.installed.tag, installed.release.tag);
        assert_eq!(
            receipt.installed.version,
            installed.release.version.map(|v| v.to_string())
        );
        assert_eq!(
            receipt.installed.commit,
            installed.release.advertised_object_id
        );
        assert_eq!(receipt.policy, WorkflowUpdatePolicy::Prompt);
        assert!(!installed.cleanup_pending);
        assert!(
            root.path()
                .join("workflows/team/build/src/workflow.ts")
                .is_file()
        );
        assert!(
            service
                .install(ManagedWorkflowInstallRequest {
                    source: &source,
                    dependency_runtime: None,
                    cancelled: &cancelled,
                })
                .is_err()
        );
        assert_eq!(
            service
                .store
                .list_receipts(/*cancelled*/ None)
                .expect("receipt after duplicate install"),
            receipts
        );
    }
}

#[test]
fn refuses_unmanaged_target_and_missing_dependency_runtime() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(
        &repository,
        /*tagged*/ false,
        /*dependencies*/ false,
    );
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

    let dependency_repository = root.path().join("dependencies");
    source(
        &dependency_repository,
        /*tagged*/ false,
        /*dependencies*/ true,
    );
    let error = service
        .install(ManagedWorkflowInstallRequest {
            source: dependency_repository.to_str().expect("UTF-8 path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect_err("occupied target should fail before Bun setup");
    assert!(
        !error
            .to_string()
            .contains("require Bun and a local sandbox"),
        "{error:#}"
    );
    fs::remove_dir_all(target.parent().expect("unmanaged target parent"))
        .expect("remove fixture target");
    let error = service
        .install(ManagedWorkflowInstallRequest {
            source: dependency_repository.to_str().expect("UTF-8 path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect_err("missing dependency runtime");
    assert!(
        error
            .to_string()
            .contains("require Bun and a local sandbox")
    );
    assert!(!target.exists());
    assert!(
        service
            .store
            .list_receipts(/*cancelled*/ None)
            .expect("receipt catalog")
            .is_empty()
    );
}
