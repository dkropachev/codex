use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::ManagedWorkflowStore;
use super::super::receipt::ManagedWorkflowReceipt;
use super::super::receipt::WorkflowRelease;
use super::super::receipt::WorkflowUpdatePolicy;
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

pub(in crate::managed::store) fn verified_release(
    with_dependencies: bool,
    synthetic_remote: bool,
) -> (
    tempfile::TempDir,
    crate::managed::integrity::VerifiedWorkflowRelease,
    ManagedWorkflowReceipt,
    AbsolutePathBuf,
) {
    let source = tempfile::tempdir().expect("source");
    fs::create_dir(source.path().join("src")).expect("source directory");
    fs::write(source.path().join("workflow.yaml"), MANIFEST).expect("manifest");
    fs::write(
        source.path().join("package.json"),
        if with_dependencies {
            r#"{"dependencies":{"dep":"1.0.0"}}"#
        } else {
            "{}"
        },
    )
    .expect("package");
    if with_dependencies {
        fs::write(
            source.path().join("bun.lock"),
            r#"{"lockfileVersion":1,"workspaces":{"":{"dependencies":{"dep":"1.0.0"}}},"packages":{"dep":["dep@1.0.0","",{},"integrity"]}}"#,
        )
        .expect("lockfile");
    }
    fs::write(
        source.path().join("src/workflow.ts"),
        "export default {};\n",
    )
    .expect("source file");
    git(source.path(), &["init", "-q"]);
    git(source.path(), &["add", "--all"]);
    git(
        source.path(),
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
    let source_url =
        crate::managed::WorkflowGitSource::parse(source.path().to_str().expect("UTF-8 path"))
            .expect("parse source");
    let release = crate::managed::resolve_workflow_git_release(&source_url).expect("resolve");
    let staging = tempfile::tempdir().expect("source staging");
    let staged = crate::managed::fetch::stage_resolved_workflow_release_cancellable(
        &absolute(staging.path()),
        &source_url,
        &release,
        &AtomicBool::new(false),
    )
    .expect("stage release");
    // The fetch fixture is local. Build a synthetic remote-source staged value
    // to exercise preparation without weakening production provenance ownership.
    let staged = if synthetic_remote {
        staged.with_synthetic_test_source(
            crate::managed::WorkflowGitSource::parse("https://example.com/team/build.git")
                .expect("fixture receipt source"),
        )
    } else {
        staged
    };
    let staged_path = staged.root().clone();
    if with_dependencies {
        fs::create_dir_all(staged_path.join("node_modules/.bin")).expect("dependency bins");
        fs::write(staged_path.join("node_modules/dep.js"), "dependency").expect("dependency");
        std::os::unix::fs::symlink("../dep.js", staged_path.join("node_modules/.bin/dep"))
            .expect("dependency link");
    }
    let verified = crate::managed::integrity::verify_post_install(
        staged,
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect("verify release");
    let receipt = ManagedWorkflowReceipt::new(
        "team/build".into(),
        "https://example.com/team/build.git".into(),
        WorkflowRelease {
            tag: release.tag,
            version: release.version.map(|version| version.to_string()),
            commit: release.advertised_object_id,
        },
        WorkflowUpdatePolicy::Prompt,
    )
    .expect("next receipt");
    (staging, verified, receipt, staged_path)
}

pub(in crate::managed::store) fn store(root: &Path) -> ManagedWorkflowStore {
    fs::create_dir(root.join("workflows")).expect("workflow root");
    ManagedWorkflowStore::create(&absolute(root), &absolute(&root.join("workflows")))
        .expect("managed store")
}

#[test]
fn prepare_copies_verified_payload_without_git_and_writes_matching_marker() {
    let root = tempfile::tempdir().expect("store root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let expected_evidence = verified.evidence().clone();
    let prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt.clone(),
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    let payload = prepared.staging.directory().path().join("payload");
    assert!(!payload.join(".git").exists());
    assert!(payload.join("src/workflow.ts").is_file());
    assert_eq!(prepared.journal.next_receipt, receipt);
    assert_eq!(prepared.journal.evidence, expected_evidence);
    let directory =
        SecureDirectory::open_root(&absolute(payload.as_path())).expect("prepared directory");
    assert!(
        super::super::journal::read_marker(&directory)
            .expect("read manager marker")
            .matches_journal(&prepared.journal)
    );
    let path = prepared.staging.directory().path().to_path_buf();
    drop(prepared);
    assert!(!path.exists());
}

#[test]
fn prepare_rejects_changed_verified_source_and_mismatched_receipt() {
    let root = tempfile::tempdir().expect("store root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source_staging, verified, receipt, staged_path) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    fs::write(staged_path.join("src/workflow.ts"), "changed").expect("tamper verified source");
    assert!(
        store
            .prepare_release(
                &lock,
                verified,
                /*previous_receipt*/ None,
                receipt,
                crate::managed::fetch::VERIFICATION_LIMITS,
                crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
                /*cancelled*/ None,
            )
            .is_err()
    );
    assert!(
        store
            .staging
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("staging entries")
            .is_empty()
    );

    let (_source_staging, verified, mut receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    receipt.installed.commit = "b".repeat(40);
    assert!(
        store
            .prepare_release(
                &lock,
                verified,
                /*previous_receipt*/ None,
                receipt,
                crate::managed::fetch::VERIFICATION_LIMITS,
                crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
                /*cancelled*/ None,
            )
            .is_err()
    );

    let (_source_staging, verified, mut receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    receipt.source = "https://example.com/different.git".into();
    assert!(
        store
            .prepare_release(
                &lock,
                verified,
                /*previous_receipt*/ None,
                receipt,
                crate::managed::fetch::VERIFICATION_LIMITS,
                crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
                /*cancelled*/ None,
            )
            .is_err()
    );
}

#[test]
fn prepare_preserves_contained_dependency_symlink() {
    let root = tempfile::tempdir().expect("store root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ true, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt,
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare dependency release");
    assert_eq!(
        fs::read_link(
            prepared
                .staging
                .directory()
                .path()
                .join("payload/node_modules/.bin/dep")
        )
        .expect("copied dependency link"),
        Path::new("../dep.js"),
    );
}

#[test]
fn prepare_rejects_local_source_before_creating_staging() {
    let root = tempfile::tempdir().expect("store root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ false,
    );
    let error = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt,
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .err()
        .expect("local source must fail");
    assert!(
        error.to_string().contains("local workflow source"),
        "{error:#}"
    );
    assert!(
        store
            .staging
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("staging entries")
            .is_empty()
    );
}

#[test]
fn published_payload_evidence_excludes_the_validated_manager_marker() {
    let root = tempfile::tempdir().expect("root");
    let store = store(root.path());
    let lock = store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("lock workflow");
    let (_source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    let prepared = store
        .prepare_release(
            &lock,
            verified,
            /*previous_receipt*/ None,
            receipt,
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .expect("prepare release");
    let payload = prepared.staging.directory().path().join("payload");
    crate::managed::integrity::verify_published_copy(
        payload.as_path(),
        &prepared.journal.evidence,
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
    .expect("published payload evidence");
    assert!(
        crate::managed::integrity::verify_materialized_copy(
            payload.as_path(),
            &prepared.journal.evidence,
            crate::managed::fetch::VERIFICATION_LIMITS,
            crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
            /*cancelled*/ None,
        )
        .is_err()
    );
}

#[test]
fn preparation_requires_a_lock_from_the_same_store() {
    let first = tempfile::tempdir().expect("first store root");
    let second = tempfile::tempdir().expect("second store root");
    let first_store = store(first.path());
    let second_store = store(second.path());
    let first_lock = first_store
        .lock_install("team/build", /*cancelled*/ None)
        .expect("first store lock");
    let (_source_staging, verified, receipt, _) = verified_release(
        /*with_dependencies*/ false, /*synthetic_remote*/ true,
    );
    assert!(
        second_store
            .prepare_release(
                &first_lock,
                verified,
                /*previous_receipt*/ None,
                receipt,
                crate::managed::fetch::VERIFICATION_LIMITS,
                crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
                /*cancelled*/ None,
            )
            .is_err()
    );
    assert!(
        second_store
            .staging
            .list_names(/*maximum_entries*/ 10, /*cancelled*/ None)
            .expect("second staging names")
            .is_empty()
    );
}
