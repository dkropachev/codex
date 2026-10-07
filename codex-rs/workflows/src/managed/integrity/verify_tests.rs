use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;

const MANIFEST: &str = "apiVersion: 1\nid: test/workflow\ntitle: Test workflow\ncallableName: test-workflow\ndescription: Test package\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n";
const TEXT_LOCK: &str = r#"{"lockfileVersion":1,"workspaces":{"":{"dependencies":{"dep":"1.0.0"}}},"packages":{"dep":["dep@1.0.0","",{},"integrity"]}}"#;

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

fn staged(with_dependencies: bool) -> (tempfile::TempDir, StagedWorkflowRelease) {
    let source = tempfile::tempdir().expect("source repository");
    fs::create_dir(source.path().join("src")).expect("source directory");
    fs::write(source.path().join("workflow.yaml"), MANIFEST).expect("workflow manifest");
    fs::write(
        source.path().join("src/workflow.ts"),
        "export default {};\n",
    )
    .expect("source file");
    fs::write(source.path().join(".gitignore"), "ignored-*\n").expect("ignore rule");
    fs::write(
        source.path().join("package.json"),
        if with_dependencies {
            r#"{"dependencies":{"dep":"1.0.0"}}"#
        } else {
            "{}"
        },
    )
    .expect("package manifest");
    if with_dependencies {
        fs::write(source.path().join("bun.lock"), TEXT_LOCK).expect("text lock");
    }
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
    let source =
        crate::managed::WorkflowGitSource::parse(source.path().to_str().expect("UTF-8 path"))
            .expect("source URL");
    let release = crate::managed::resolve_workflow_git_release(&source).expect("resolve release");
    let staging = tempfile::tempdir().expect("staging root");
    let root =
        codex_utils_absolute_path::AbsolutePathBuf::from_absolute_path_checked(staging.path())
            .expect("absolute staging root");
    let staged = crate::managed::fetch::stage_resolved_workflow_release_cancellable(
        &root,
        &source,
        &release,
        &AtomicBool::new(false),
    )
    .expect("stage release");
    (staging, staged)
}

fn verify(staged: StagedWorkflowRelease) -> anyhow::Result<VerifiedWorkflowRelease> {
    verify_post_install(
        staged,
        crate::managed::fetch::VERIFICATION_LIMITS,
        crate::runner::CommandDeadline::after(Duration::from_secs(/*secs*/ 5)),
        /*cancelled*/ None,
    )
}

#[test]
fn post_install_rejects_tracked_and_added_source_changes() {
    let mut changes = vec![
        "edit",
        "delete",
        "stage",
        "ignored",
        "head",
        "index",
        "extra-node-modules",
    ];
    #[cfg(unix)]
    changes.push("mode");
    for change in changes {
        let (_staging, staged) = staged(/*with_dependencies*/ false);
        let root = staged.root().as_path();
        match change {
            "edit" => fs::write(root.join("workflow.yaml"), "changed").expect("edit source"),
            "delete" => fs::remove_file(root.join("src/workflow.ts")).expect("delete source"),
            "stage" => {
                fs::write(root.join("package.json"), "{\"name\":\"changed\"}")
                    .expect("edit package");
                git(root, &["add", "package.json"]);
            }
            "ignored" => {
                fs::write(root.join("ignored-output"), "unexpected").expect("ignored output")
            }
            "head" => {
                fs::write(root.join(".git/HEAD"), "ref: refs/heads/missing\n").expect("change HEAD")
            }
            "index" => fs::write(root.join(".git/index"), "invalid index").expect("replace index"),
            "extra-node-modules" => {
                fs::create_dir(root.join("node_modules")).expect("unexpected dependencies")
            }
            #[cfg(unix)]
            "mode" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    root.join("workflow.yaml"),
                    fs::Permissions::from_mode(0o755),
                )
                .expect("change executable mode");
            }
            _ => unreachable!(),
        }
        assert!(verify(staged).is_err(), "{change}");
    }
}

#[test]
fn evidence_is_canonical_and_binds_every_entry_property() {
    let file = super::super::PayloadEntry {
        path: "file".into(),
        kind: PayloadEntryKind::File {
            executable: false,
            sha256: [1; 32],
        },
    };
    let empty = super::super::PayloadEntry {
        path: "empty".into(),
        kind: PayloadEntryKind::Directory,
    };
    let link = super::super::PayloadEntry {
        path: "node_modules/.bin/tool".into(),
        kind: PayloadEntryKind::DependencyLink {
            target: "../pkg/tool".into(),
        },
    };
    let base = PayloadInventory {
        entries: vec![file.clone(), empty.clone(), link.clone()],
        logical_bytes: 8,
    };
    let expected = evidence_for_inventory(&base).expect("base evidence");
    assert_eq!(
        evidence_for_inventory(&PayloadInventory {
            entries: vec![link.clone(), empty.clone(), file.clone()],
            logical_bytes: 8,
        })
        .expect("reordered evidence"),
        expected,
    );
    for changed in [
        vec![file.clone(), link.clone()],
        vec![
            super::super::PayloadEntry {
                path: "renamed".into(),
                ..file.clone()
            },
            empty.clone(),
            link.clone(),
        ],
        vec![
            super::super::PayloadEntry {
                kind: PayloadEntryKind::Directory,
                ..file.clone()
            },
            empty.clone(),
            link.clone(),
        ],
        vec![
            super::super::PayloadEntry {
                kind: PayloadEntryKind::File {
                    executable: true,
                    sha256: [1; 32],
                },
                ..file.clone()
            },
            empty.clone(),
            link.clone(),
        ],
        vec![
            super::super::PayloadEntry {
                kind: PayloadEntryKind::File {
                    executable: false,
                    sha256: [2; 32],
                },
                ..file.clone()
            },
            empty.clone(),
            link.clone(),
        ],
        vec![
            file,
            empty,
            super::super::PayloadEntry {
                kind: PayloadEntryKind::DependencyLink {
                    target: "../other/tool".into(),
                },
                ..link
            },
        ],
    ] {
        assert_ne!(
            evidence_for_inventory(&PayloadInventory {
                entries: changed,
                logical_bytes: 8
            })
            .expect("changed evidence"),
            expected
        );
    }
}
