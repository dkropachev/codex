#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

const MANIFEST: &str = "apiVersion: 1\nid: team/build\ntitle: Team Build\ncallableName: team-build\ndescription: Build workflow\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute fixture path")
}

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

fn commit(root: &Path) {
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
            "release",
        ],
    );
}

fn source(root: &Path, version: Option<&str>) {
    fs::create_dir(root).expect("source root");
    fs::create_dir(root.join("src")).expect("source directory");
    fs::write(root.join("workflow.yaml"), MANIFEST).expect("manifest");
    fs::write(
        root.join("package.json"),
        version.map_or("{}".to_string(), |version| {
            format!("{{\"version\":\"{version}\"}}")
        }),
    )
    .expect("package");
    fs::write(root.join("src/workflow.ts"), "export default {};\n").expect("workflow source");
    git(root, &["init", "-q"]);
    commit(root);
    if let Some(version) = version {
        git(root, &["tag", &format!("v{version}")]);
    }
}

fn service(root: &Path) -> ManagedWorkflowService {
    ManagedWorkflowService::new(&absolute(root), &absolute(&root.join("workflows")))
        .expect("service")
}

fn install(service: &ManagedWorkflowService, root: &Path, cancelled: &AtomicBool) {
    service
        .install(super::super::ManagedWorkflowInstallRequest {
            source: root.to_str().expect("UTF-8 source"),
            dependency_runtime: None,
            cancelled,
        })
        .expect("install initial release");
}

#[test]
fn check_reports_newer_stable_release_without_mutation() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, Some("1.0.0"));
    let service = service(root.path());
    let cancelled = AtomicBool::new(false);
    install(&service, &repository, &cancelled);
    let initial = service
        .check_update("team/build", &cancelled)
        .expect("initial update check");
    assert_eq!(initial.update, ManagedWorkflowUpdate::Current);

    fs::write(repository.join("package.json"), r#"{"version":"1.1.0"}"#)
        .expect("new package version");
    commit(&repository);
    git(&repository, &["tag", "v1.1.0"]);
    let checked = service
        .check_update("team/build", &cancelled)
        .expect("new release check");
    let ManagedWorkflowUpdate::Available { release, dismissed } = checked.update else {
        panic!("expected available release");
    };
    assert_eq!(release.tag.as_deref(), Some("v1.1.0"));
    assert!(!dismissed);
    assert_eq!(
        service.list_installed().expect("receipt catalog")[0].installed,
        initial.workflow.installed
    );
}

#[test]
fn check_reports_moved_tag_and_downgrade_as_errors() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, Some("2.0.0"));
    let service = service(root.path());
    let cancelled = AtomicBool::new(false);
    install(&service, &repository, &cancelled);
    fs::write(
        repository.join("src/workflow.ts"),
        "export default { changed: true };\n",
    )
    .expect("change source");
    commit(&repository);
    git(&repository, &["tag", "-f", "v2.0.0"]);
    let moved = service
        .check_update("team/build", &cancelled)
        .expect("moved tag check");
    assert!(
        matches!(moved.update, ManagedWorkflowUpdate::Error(message) if message.contains("tag moved"))
    );
    fs::write(repository.join("package.json"), r#"{"version":"3.0.0"}"#)
        .expect("newer package version");
    commit(&repository);
    git(&repository, &["tag", "v3.0.0"]);
    let moved_with_newer = service
        .check_update("team/build", &cancelled)
        .expect("moved installed tag with newer release");
    assert!(
        matches!(moved_with_newer.update, ManagedWorkflowUpdate::Error(message) if message.contains("tag moved"))
    );
    git(&repository, &["tag", "-d", "v3.0.0"]);
    git(&repository, &["tag", "-d", "v2.0.0"]);
    git(&repository, &["tag", "v1.0.0"]);
    fs::write(repository.join("package.json"), r#"{"version":"1.0.0"}"#)
        .expect("downgraded package version");
    commit(&repository);
    git(&repository, &["tag", "-f", "v1.0.0"]);
    let downgrade = service
        .check_update("team/build", &cancelled)
        .expect("downgrade check");
    assert!(
        matches!(downgrade.update, ManagedWorkflowUpdate::Error(message) if message.contains("downgrade"))
    );
}

#[test]
fn equivalent_tag_alias_at_the_same_commit_is_current() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, Some("1.0.0"));
    let service = service(root.path());
    let cancelled = AtomicBool::new(false);
    install(&service, &repository, &cancelled);
    git(&repository, &["tag", "1.0.0"]);
    assert_eq!(
        service
            .check_update("team/build", &cancelled)
            .expect("alias check")
            .update,
        ManagedWorkflowUpdate::Current
    );
}

#[test]
fn missing_local_source_is_visible_without_corrupting_receipt() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, /*version*/ None);
    let service = service(root.path());
    let cancelled = AtomicBool::new(false);
    install(&service, &repository, &cancelled);
    let before = service.list_installed().expect("catalog");
    fs::rename(&repository, root.path().join("moved-source")).expect("move local repository");
    let check = service
        .check_update("team/build", &cancelled)
        .expect("missing source check");
    assert!(
        matches!(check.update, ManagedWorkflowUpdate::Error(message) if message.contains("source is unavailable"))
    );
    assert_eq!(
        service.list_installed().expect("catalog after move"),
        before
    );
}

#[test]
fn dismissal_matches_only_the_exact_release() {
    let root = tempfile::tempdir().expect("root");
    let repository = root.path().join("source");
    source(&repository, Some("1.0.0"));
    let source =
        WorkflowGitSource::parse(repository.to_str().expect("UTF-8 source")).expect("source");
    let initial =
        super::super::super::resolve_workflow_git_release(&source).expect("initial release");
    let mut receipt = ManagedWorkflowReceipt::new(
        "team/build".into(),
        source.receipt_source().expect("stored source"),
        crate::managed::store::WorkflowRelease {
            tag: initial.tag,
            version: initial.version.map(|version| version.to_string()),
            commit: initial.advertised_object_id,
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("receipt");
    fs::write(repository.join("package.json"), r#"{"version":"1.1.0"}"#)
        .expect("new package version");
    commit(&repository);
    git(&repository, &["tag", "v1.1.0"]);
    let selected =
        super::super::super::resolve_workflow_git_release(&source).expect("selected release");
    receipt.dismissed_release = Some(crate::managed::store::WorkflowRelease {
        tag: selected.tag,
        version: selected.version.map(|version| version.to_string()),
        commit: selected.advertised_object_id,
    });
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        check_release(&receipt, &cancelled).expect("dismissed check"),
        ManagedWorkflowUpdate::Available {
            dismissed: true,
            ..
        }
    ));
    fs::write(repository.join("package.json"), r#"{"version":"1.2.0"}"#)
        .expect("newer package version");
    commit(&repository);
    git(&repository, &["tag", "v1.2.0"]);
    assert!(matches!(
        check_release(&receipt, &cancelled).expect("newer release check"),
        ManagedWorkflowUpdate::Available {
            dismissed: false,
            ..
        }
    ));
}
