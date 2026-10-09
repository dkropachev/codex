#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;
use crate::managed::management::ManagedWorkflowInstallRequest;
use crate::managed::management::WorkflowUpdatePolicy;

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

fn commit_release(root: &Path, version: &str) {
    fs::write(
        root.join("package.json"),
        format!(r#"{{"version":"{version}"}}"#),
    )
    .expect("package");
    fs::write(
        root.join("src/workflow.ts"),
        format!("export default '{version}';\n"),
    )
    .expect("source");
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
            version,
        ],
    );
    git(root, &["tag", &format!("v{version}")]);
}

fn fixture() -> (
    tempfile::TempDir,
    ManagedWorkflowService,
    WorkflowReleaseIdentity,
) {
    let root = tempfile::tempdir().expect("root");
    let source = root.path().join("source");
    fs::create_dir(&source).expect("source directory");
    fs::create_dir(source.join("src")).expect("source directory");
    fs::write(source.join("workflow.yaml"), MANIFEST).expect("manifest");
    git(&source, &["init", "-q"]);
    commit_release(&source, "1.0.0");
    let home = AbsolutePathBuf::from_absolute_path_checked(root.path()).expect("home");
    let workflows = AbsolutePathBuf::from_absolute_path_checked(root.path().join("workflows"))
        .expect("workflows");
    let service = ManagedWorkflowService::new(&home, &workflows).expect("service");
    let cancelled = AtomicBool::new(false);
    service
        .install(ManagedWorkflowInstallRequest {
            source: source.to_str().expect("source path"),
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("install");
    let installed = service.list_installed().expect("installed")[0]
        .installed
        .clone();
    (root, service, installed)
}

fn available(service: &ManagedWorkflowService) -> WorkflowReleaseIdentity {
    let cancelled = AtomicBool::new(false);
    let ManagedWorkflowUpdate::Available { release, .. } = service
        .check_update("team/build", &cancelled)
        .expect("check update")
        .update
    else {
        panic!("expected available release");
    };
    release
}

#[test]
fn explicit_update_replaces_a_dismissed_release_and_retains_policy() {
    let (root, service, installed) = fixture();
    commit_release(&root.path().join("source"), "1.1.0");
    let release = available(&service);
    let cancelled = AtomicBool::new(false);
    service
        .set_policy(
            "team/build",
            &installed,
            WorkflowUpdatePolicy::Manual,
            &cancelled,
        )
        .expect("manual policy");
    service
        .dismiss_release("team/build", &installed, &release, &cancelled)
        .expect("dismiss release");
    let updated = service
        .update(ManagedWorkflowUpdateRequest {
            id: "team/build",
            expected_installed: &installed,
            expected_available: &release,
            dependency_runtime: None,
            cancelled: &cancelled,
        })
        .expect("explicit update");
    assert_eq!(
        updated.release.version.as_ref().map(ToString::to_string),
        Some("1.1.0".into())
    );
    let record = &service.list_installed().expect("updated records")[0];
    assert_eq!(record.installed, release);
    assert_eq!(record.policy, WorkflowUpdatePolicy::Manual);
    assert_eq!(record.dismissed_release, None);
    assert_eq!(
        fs::read_to_string(root.path().join("workflows/team/build/src/workflow.ts"))
            .expect("active source"),
        "export default '1.1.0';\n"
    );
}

#[test]
fn update_rejects_dirty_payload_and_stale_release_without_replacing_it() {
    let (root, service, installed) = fixture();
    commit_release(&root.path().join("source"), "1.1.0");
    let release = available(&service);
    let cancelled = AtomicBool::new(false);
    let stale = WorkflowReleaseIdentity {
        tag: Some("v9.9.9".into()),
        version: Some("9.9.9".into()),
        commit: release.commit.clone(),
    };
    assert!(
        service
            .update(ManagedWorkflowUpdateRequest {
                id: "team/build",
                expected_installed: &installed,
                expected_available: &stale,
                dependency_runtime: None,
                cancelled: &cancelled,
            })
            .expect_err("stale available release")
            .to_string()
            .contains("available release changed")
    );
    let active = root.path().join("workflows/team/build/src/workflow.ts");
    fs::write(&active, "modified locally\n").expect("modify payload");
    assert!(
        service
            .update(ManagedWorkflowUpdateRequest {
                id: "team/build",
                expected_installed: &installed,
                expected_available: &release,
                dependency_runtime: None,
                cancelled: &cancelled,
            })
            .is_err()
    );
    assert_eq!(
        service.list_installed().expect("records")[0].installed,
        installed
    );
    assert_eq!(
        fs::read_to_string(active).expect("active source"),
        "modified locally\n"
    );
}
