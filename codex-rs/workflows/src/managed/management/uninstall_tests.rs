#![allow(clippy::expect_used)]

use std::fs;
use std::process::Command;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;
use crate::managed::management::ManagedWorkflowInstallRequest;
use crate::managed::management::WorkflowUpdatePolicy;

#[test]
fn uninstall_requires_the_current_release_and_removes_its_receipt() {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir(&source).expect("source");
    fs::write(
        source.join("workflow.yaml"),
        "apiVersion: 1\nid: team/build\ntitle: Team Build\ncallableName: team-build\ndescription: Build workflow\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
    )
    .expect("manifest");
    fs::write(source.join("package.json"), "{}").expect("package");
    fs::create_dir(source.join("src")).expect("source tree");
    fs::write(source.join("src/workflow.ts"), "export default {};\n").expect("source code");
    for args in [
        &["init", "-q"][..],
        &["add", "--all"][..],
        &["-c", "commit.gpgsign=false", "commit", "-qm", "initial"][..],
    ] {
        let output = Command::new("git")
            .current_dir(&source)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .args(args)
            .output()
            .expect("Git");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let home = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("home");
    let workflows = AbsolutePathBuf::from_absolute_path_checked(root.path().join("workflows"))
        .expect("workflows");
    let service = ManagedWorkflowService::new(&home, &workflows).expect("service");
    let cancelled = AtomicBool::new(false);
    service
        .install(ManagedWorkflowInstallRequest {
            source: source.to_str().expect("source path"),
            policy: WorkflowUpdatePolicy::Manual,
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("install managed workflow");
    let record = &service.list_installed().expect("records")[0];
    assert_eq!(record.policy, WorkflowUpdatePolicy::Manual);
    let installed = record.installed.clone();
    let stale = WorkflowReleaseIdentity {
        commit: "0000000000000000000000000000000000000000".into(),
        ..installed.clone()
    };
    assert!(service.uninstall("team/build", &stale, &cancelled).is_err());
    assert_eq!(
        service.list_installed().expect("records")[0].installed,
        installed
    );
    let removed = service
        .uninstall("team/build", &installed, &cancelled)
        .expect("uninstall managed workflow");
    assert_eq!(
        removed,
        ManagedWorkflowUninstallation {
            id: "team/build".into(),
            cleanup_pending: false,
        }
    );
    assert!(service.list_installed().expect("records").is_empty());
    assert!(!root.path().join("workflows/team/build").exists());
}
