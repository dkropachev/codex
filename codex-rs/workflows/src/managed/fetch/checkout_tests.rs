use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::super::ResolvedWorkflowRelease;
use super::super::VERIFICATION_LIMITS;
use super::super::WorkflowGitSource;
use super::super::fetch_with_options;
use super::*;

struct Repository(tempfile::TempDir);

impl Repository {
    fn new(version: Option<serde_json::Value>) -> Self {
        let temporary = tempfile::tempdir().expect("temporary repository");
        fs::create_dir_all(temporary.path().join("src")).expect("create source directory");
        fs::create_dir_all(temporary.path().join("state")).expect("create state directory");
        fs::write(
            temporary.path().join("workflow.yaml"),
            "apiVersion: 1\nid: test/workflow\ntitle: Test workflow\ncallableName: test-workflow\ndescription: Test workflow package\nvalidation:\n  commands: []\n  coverage:\n    positive: true\n    load: true\n    autocomplete: true\n    negative: true\n",
        )
        .expect("write workflow manifest");
        let version = version.map_or(String::new(), |version| {
            format!(r#", "version": {version}"#)
        });
        let package =
            format!(r#"{{"name":"@test/workflow","private":true,"type":"module"{version}}}"#);
        fs::write(temporary.path().join("package.json"), package).expect("write package");
        fs::write(
            temporary.path().join("src/workflow.ts"),
            "export default {};\n",
        )
        .expect("write workflow source");
        fs::write(temporary.path().join("state/.gitkeep"), "").expect("write state marker");
        git(temporary.path(), ["init", "-q"]);
        git(temporary.path(), ["add", "--all"]);
        git(temporary.path(), ["commit", "-qm", "package"]);
        Self(temporary)
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn resolve(&self, tag: Option<&str>) -> (WorkflowGitSource, ResolvedWorkflowRelease) {
        if let Some(tag) = tag {
            git(self.root(), ["tag", tag]);
        }
        let source = WorkflowGitSource::parse(self.root().to_str().expect("UTF-8 repository path"))
            .expect("parse repository source");
        let release = super::super::super::git_command::resolve_workflow_git_release(
            &source, /*cancelled*/ None,
        )
        .expect("resolve release");
        (source, release)
    }

    fn fetch(&self, tag: Option<&str>) -> (tempfile::TempDir, FetchedWorkflowRelease) {
        let (source, release) = self.resolve(tag);
        let staging = tempfile::tempdir().expect("temporary staging root");
        let fetched = fetch_with_options(
            OsStr::new("git"),
            &absolute(staging.path()),
            &source,
            &release,
            /*cancelled*/ None,
            VERIFICATION_LIMITS,
        )
        .expect("fetch release");
        (staging, fetched)
    }
}

#[test]
fn checks_out_detached_tag_and_snapshot_packages() {
    let temporary = tempfile::tempdir().expect("temporary checkout command");
    let command = checkout_command(OsStr::new("git"), temporary.path(), temporary.path(), "1");
    assert!(command.get_envs().any(|(name, value)| {
        name == OsStr::new("GIT_NO_LAZY_FETCH") && value == Some(OsStr::new("1"))
    }));
    for (tag, version) in [
        (Some("v1.2.3"), Some(serde_json::json!("1.2.3"))),
        (None, Some(serde_json::json!(123))),
    ] {
        let repository = Repository::new(version);
        let (source, expected) = repository.resolve(tag);
        let staging = tempfile::tempdir().expect("temporary staging root");
        let staged = super::super::stage_resolved_workflow_release_cancellable(
            &absolute(staging.path()),
            &source,
            &expected,
            &AtomicBool::new(false),
        )
        .expect("stage release");

        assert_eq!(staged.release(), &expected);
        assert_eq!(staged.baseline().commit, expected.advertised_object_id);
        assert_eq!(
            staged
                .baseline()
                .index
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            [
                "package.json",
                "src/workflow.ts",
                "state/.gitkeep",
                "workflow.yaml"
            ]
        );
        assert_eq!(
            staged.dependencies(),
            &crate::managed::dependencies::ValidatedManagedDependencies {
                sources: crate::managed::dependencies::ValidatedDependencySources {
                    has_dependencies: false,
                    local_packages: Vec::new(),
                },
                lockfile: crate::managed::dependencies::ManagedBunLockfile::NotRequired,
            }
        );
        assert!(staged.root().join("workflow.yaml").is_file());
        assert!(staged.root().join("state/.gitkeep").is_file());
        assert_eq!(
            git(staged.root(), ["rev-parse", "HEAD"]),
            expected.advertised_object_id
        );
        assert!(
            !git_output(staged.root(), ["symbolic-ref", "-q", "HEAD"])
                .status
                .success()
        );
        fs::write(staged.root().join("workflow.yaml"), "modified").expect("modify tracked source");
        assert!(
            crate::managed::integrity::capture_source_baseline(
                OsStr::new("git"),
                staged.root().as_path(),
                &expected.advertised_object_id,
                VERIFICATION_LIMITS,
                /*cancelled*/ None,
            )
            .is_err()
        );
        git(staged.root(), ["add", "workflow.yaml"]);
        assert!(
            crate::managed::integrity::capture_source_baseline(
                OsStr::new("git"),
                staged.root().as_path(),
                &expected.advertised_object_id,
                VERIFICATION_LIMITS,
                /*cancelled*/ None,
            )
            .is_err()
        );
    }
}

#[test]
fn validates_dependency_lock_policy_before_returning_a_staged_release() {
    let repository = Repository::new(/*version*/ None);
    fs::write(
        repository.root().join("package.json"),
        r#"{"name":"@test/workflow","private":true,"type":"module","dependencies":{"dep":"1.2.3"}}"#,
    )
    .expect("write dependency manifest");
    fs::write(
        repository.root().join("bun.lock"),
        r#"{
          "lockfileVersion": 1,
          "configVersion": 1,
          "workspaces": {"": {"dependencies": {"dep": "1.2.3"}}},
          "packages": {"dep": ["dep@1.2.3", "", {}, ""]}
        }"#,
    )
    .expect("write Bun lock");
    git(repository.root(), ["add", "package.json", "bun.lock"]);
    git(repository.root(), ["commit", "-qm", "add dependencies"]);

    let (source, release) = repository.resolve(/*tag*/ None);
    let staging = tempfile::tempdir().expect("temporary staging root");
    let staged = super::super::stage_resolved_workflow_release_cancellable(
        &absolute(staging.path()),
        &source,
        &release,
        &AtomicBool::new(false),
    )
    .expect("stage dependency package");
    assert_eq!(
        staged.dependencies(),
        &crate::managed::dependencies::ValidatedManagedDependencies {
            sources: crate::managed::dependencies::ValidatedDependencySources {
                has_dependencies: true,
                local_packages: Vec::new(),
            },
            lockfile: crate::managed::dependencies::ManagedBunLockfile::TextSourcesValidated,
        }
    );

    git(repository.root(), ["rm", "bun.lock"]);
    git(repository.root(), ["commit", "-qm", "remove lock"]);
    let (source, release) = repository.resolve(/*tag*/ None);
    let staging = tempfile::tempdir().expect("temporary staging root");
    let error = super::super::stage_resolved_workflow_release_cancellable(
        &absolute(staging.path()),
        &source,
        &release,
        &AtomicBool::new(false),
    )
    .err()
    .expect("dependency package without lock must fail");
    assert!(
        format!("{error:#}").contains("dependencies require bun.lock or bun.lockb"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn tagged_release_requires_exact_string_semver_version() {
    for (version, expected) in [
        (
            None,
            "tagged workflow release requires a string package.json version",
        ),
        (
            Some(serde_json::json!(123)),
            "tagged workflow release requires a string package.json version",
        ),
        (
            Some(serde_json::json!("not-semver")),
            "workflow package version `not-semver` is not SemVer",
        ),
        (
            Some(serde_json::json!("1.2.4")),
            "workflow package version `1.2.4` does not match release version `1.2.3`",
        ),
    ] {
        let repository = Repository::new(version);
        let (_staging, fetched) = repository.fetch(Some("v1.2.3"));
        let error = checkout_fetched_release(
            OsStr::new("git"),
            fetched,
            VERIFICATION_LIMITS,
            /*cancelled*/ None,
        )
        .err()
        .expect("invalid package version should fail");
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn snapshot_still_requires_a_canonical_workflow_package() {
    let repository = Repository::new(/*version*/ None);
    git(repository.root(), ["rm", "workflow.yaml"]);
    git(repository.root(), ["commit", "-qm", "remove manifest"]);
    let (_staging, fetched) = repository.fetch(/*tag*/ None);
    let error = checkout_fetched_release(
        OsStr::new("git"),
        fetched,
        VERIFICATION_LIMITS,
        /*cancelled*/ None,
    )
    .err()
    .expect("invalid snapshot package should fail");
    assert!(
        error
            .to_string()
            .contains("missing required regular package file")
    );
}

#[test]
fn enforces_post_checkout_worktree_and_staging_limits() {
    let repository = Repository::new(Some(serde_json::json!("1.2.3")));
    for (limits, expected) in [
        (
            VerificationLimits {
                worktree_entries: 1,
                ..VERIFICATION_LIMITS
            },
            "worktree exceeds 1 entries",
        ),
        (
            VerificationLimits {
                staging_bytes: 1,
                ..VERIFICATION_LIMITS
            },
            "staging exceeds 1 bytes",
        ),
        (
            VerificationLimits {
                staging_entries: 1,
                ..VERIFICATION_LIMITS
            },
            "staging exceeds 1 entries",
        ),
    ] {
        let (_staging, fetched) = repository.fetch(/*tag*/ None);
        let error =
            checkout_fetched_release(OsStr::new("git"), fetched, limits, /*cancelled*/ None)
                .err()
                .expect("post-checkout limit should fail");
        assert!(format!("{error:#}").contains(expected));
    }
}

#[cfg(unix)]
#[test]
fn worktree_scan_and_checkout_cancellation_fail_closed() {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let root = tempfile::tempdir().expect("temporary worktree");
    fs::create_dir(root.path().join(".git")).expect("create Git directory");
    fs::create_dir(root.path().join("nested")).expect("create nested directory");
    fs::write(root.path().join("nested/file"), "data").expect("write nested file");
    let exact_entries = 2;
    inspect_worktree(root.path(), exact_entries, /*cancelled*/ None)
        .expect("exact worktree entry limit should pass");
    let error = inspect_worktree(
        root.path(),
        /*maximum_entries*/ 1,
        /*cancelled*/ None,
    )
    .expect_err("worktree entry limit should fail");
    assert!(error.to_string().contains("exceeds 1 entries"));
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    symlink(outside.path(), root.path().join("nested/link")).expect("create symlink");
    let error = inspect_worktree(
        root.path(),
        /*maximum_entries*/ 4,
        /*cancelled*/ None,
    )
    .expect_err("worktree symlink should fail");
    assert!(error.to_string().contains("symbolic link"));
    fs::remove_file(root.path().join("nested/link")).expect("remove symlink");
    let _socket = UnixListener::bind(root.path().join("nested/socket")).expect("create socket");
    let error = inspect_worktree(
        root.path(),
        /*maximum_entries*/ 4,
        /*cancelled*/ None,
    )
    .expect_err("worktree special file should fail");
    assert!(error.to_string().contains("special file"));
    let error = inspect_worktree(
        root.path(),
        /*maximum_entries*/ 4,
        Some(&AtomicBool::new(true)),
    )
    .expect_err("cancelled worktree scan should fail");
    assert!(format!("{error:#}").contains("cancelled"));

    let repository = Repository::new(Some(serde_json::json!("1.2.3")));
    let (_staging, fetched) = repository.fetch(/*tag*/ None);
    let sentinel = repository.root().join("spawned");
    let fake_git = repository.root().join("fake-git");
    fs::write(
        &fake_git,
        format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
    )
    .expect("write fake Git");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))
        .expect("make fake Git executable");
    let error = checkout_fetched_release(
        fake_git.as_os_str(),
        fetched,
        VERIFICATION_LIMITS,
        Some(&AtomicBool::new(true)),
    )
    .err()
    .expect("cancelled checkout should fail");
    assert!(format!("{error:#}").contains("cancelled"));
    assert!(!sentinel.exists());
}

#[cfg(windows)]
#[test]
fn worktree_scan_rejects_windows_junctions() {
    let root = tempfile::tempdir().expect("temporary worktree");
    let target = root.path().join("target");
    let junction = root.path().join("junction");
    fs::create_dir(&target).expect("create junction target");
    let output = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .expect("create directory junction");
    assert!(output.status.success(), "mklink /J failed");
    let error = inspect_worktree(
        root.path(),
        /*maximum_entries*/ 4,
        /*cancelled*/ None,
    )
    .expect_err("worktree junction should fail");
    assert!(error.to_string().contains("reparse point"));
    fs::remove_dir(junction).expect("remove junction");
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute test path")
}

fn git<I, S>(repository: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git_output(repository, args);
    assert!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_string()
}

fn git_output<I, S>(repository: &Path, args: I) -> std::process::Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .env("GIT_AUTHOR_NAME", "Codex Workflow Test")
        .env("GIT_AUTHOR_EMAIL", "workflow-tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "Codex Workflow Test")
        .env("GIT_COMMITTER_EMAIL", "workflow-tests@example.invalid")
        .arg("-C")
        .arg(repository)
        .args(["-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false"])
        .args(args)
        .output()
        .expect("run Git fixture command")
}
