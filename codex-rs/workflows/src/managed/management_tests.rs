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
