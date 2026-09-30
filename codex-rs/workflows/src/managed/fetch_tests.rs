use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use super::*;

enum ReleaseKind {
    Snapshot,
    Lightweight,
    Annotated,
}

struct Repository(tempfile::TempDir);

impl Repository {
    fn new(object_format: &str) -> Self {
        let temporary = tempfile::tempdir().expect("temporary repository");
        let object_format = format!("--object-format={object_format}");
        git(temporary.path(), ["init", "-q", object_format.as_str()]);
        fs::write(temporary.path().join("base"), "base\n").expect("write base fixture");
        git(temporary.path(), ["add", "base"]);
        git(temporary.path(), ["commit", "--no-gpg-sign", "-qm", "base"]);
        fs::write(temporary.path().join("README.md"), "workflow\n").expect("write fixture");
        git(temporary.path(), ["add", "README.md"]);
        git(
            temporary.path(),
            ["commit", "--no-gpg-sign", "-qm", "initial"],
        );
        Self(temporary)
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn source(&self) -> WorkflowGitSource {
        WorkflowGitSource::parse(self.root().to_str().expect("UTF-8 repository path"))
            .expect("parse repository source")
    }

    fn resolve(&self) -> ResolvedWorkflowRelease {
        super::super::git_command::resolve_workflow_git_release(
            &self.source(),
            /*cancelled*/ None,
        )
        .expect("resolve release")
    }

    fn tag(&self, kind: &ReleaseKind) {
        match kind {
            ReleaseKind::Snapshot => {}
            ReleaseKind::Lightweight => drop(git(self.root(), ["tag", "v1.2.3", "HEAD^"])),
            ReleaseKind::Annotated => drop(git(
                self.root(),
                ["tag", "--no-sign", "-am", "release", "v1.2.3", "HEAD^"],
            )),
        }
    }
}

#[test]
fn fetches_lightweight_annotated_and_sha256_snapshot_releases() {
    for (kind, object_format) in [
        (ReleaseKind::Lightweight, "sha1"),
        (ReleaseKind::Annotated, "sha1"),
        (ReleaseKind::Snapshot, "sha256"),
    ] {
        let repository = Repository::new(object_format);
        repository.tag(&kind);
        let release = repository.resolve();
        let staging = tempfile::tempdir().expect("temporary staging root");
        let command = fetch_command(
            OsStr::new("git"),
            staging.path(),
            &staging.path().join("repository"),
            &release,
            BLOB_FILTER_BYTES,
        );
        let args = command.get_args().collect::<Vec<_>>();
        let selected = release
            .tag
            .as_deref()
            .map_or("HEAD".to_string(), |tag| format!("refs/tags/{tag}"));
        let refspec = format!("+{selected}:{RELEASE_REF}");
        let tail = args[args.len() - 13..]
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let expected = format!(
            "fetch --quiet --atomic --depth=1 --no-tags --no-recurse-submodules --no-write-fetch-head --no-auto-maintenance --no-write-commit-graph --refmap= --filter=blob:limit=134217729 {SOURCE_REMOTE} {refspec}"
        );
        assert_eq!(tail, expected);
        let fetched = fetch_with_options(
            OsStr::new("git"),
            &absolute(staging.path()),
            &repository.source(),
            &release,
            /*cancelled*/ None,
            BLOB_FILTER_BYTES,
        )
        .expect("fetch exact release");

        assert_eq!(fetched.release, release);
        assert_eq!(git(&fetched.repository, ["remote"]), "");
        assert_eq!(
            git(&fetched.repository, ["for-each-ref", "--format=%(refname)"]),
            RELEASE_REF
        );
        assert_eq!(
            git(&fetched.repository, ["rev-list", "--count", RELEASE_REF]),
            "1"
        );
        let config = git(&fetched.repository, ["config", "--local", "--list"]);
        assert!(!config.to_ascii_lowercase().contains("remote."));
        assert!(!config.to_ascii_lowercase().contains("partialclone"));
    }
}

#[cfg(unix)]
#[test]
fn cancellation_and_staging_root_checks_fail_closed() {
    use std::os::unix::fs::symlink;

    let repository = Repository::new("sha1");
    let staging = tempfile::tempdir().expect("temporary staging root");
    let sentinel = repository.root().join("spawned");
    let fake_git = repository.root().join("fake-git");
    write_executable(&fake_git, &format!("touch '{}'", sentinel.display()));
    let error = fetch_with_options(
        fake_git.as_os_str(),
        &absolute(staging.path()),
        &repository.source(),
        &repository.resolve(),
        Some(&AtomicBool::new(true)),
        BLOB_FILTER_BYTES,
    )
    .err()
    .expect("cancelled fetch should fail");
    assert!(format!("{error:#}").contains("cancelled"));
    assert_eq!(
        fs::read_dir(staging.path()).expect("read staging").count(),
        0
    );
    assert!(!sentinel.exists());

    let parent = tempfile::tempdir().expect("temporary staging parent");
    let target = parent.path().join("target");
    let link = parent.path().join("link");
    fs::create_dir(&target).expect("create staging target");
    symlink(&target, &link).expect("create staging symlink");
    let error = fetch_with_options(
        OsStr::new("git"),
        &absolute(&link),
        &repository.source(),
        &repository.resolve(),
        /*cancelled*/ None,
        BLOB_FILTER_BYTES,
    )
    .err()
    .expect("symlinked staging root should fail");
    assert!(format!("{error:#}").contains("regular directory"));
}

#[test]
fn rejects_changed_and_non_commit_selected_refs() {
    let moved = Repository::new("sha1");
    moved.tag(&ReleaseKind::Lightweight);
    let release = moved.resolve();
    fs::write(moved.root().join("next"), "next\n").expect("write next revision");
    git(moved.root(), ["add", "next"]);
    git(moved.root(), ["commit", "--no-gpg-sign", "-qm", "next"]);
    git(moved.root(), ["tag", "--force", "v1.2.3", "HEAD"]);
    assert_error(
        &moved,
        &release,
        BLOB_FILTER_BYTES,
        "changed before it could be fetched",
    );

    let blob_tag = Repository::new("sha1");
    fs::write(blob_tag.root().join("blob"), "not a commit\n").expect("write blob");
    let blob = git(blob_tag.root(), ["hash-object", "-w", "blob"]);
    git(blob_tag.root(), ["tag", "v1.2.3", &blob]);
    let release = blob_tag.resolve();
    assert_error(
        &blob_tag,
        &release,
        BLOB_FILTER_BYTES,
        "commit inspection failed",
    );

    let blob_head = Repository::new("sha1");
    fs::write(blob_head.root().join("blob"), "not a commit\n").expect("write blob");
    let blob = git(blob_head.root(), ["hash-object", "-w", "blob"]);
    fs::write(blob_head.root().join(".git/HEAD"), format!("{blob}\n")).expect("write blob HEAD");
    let release = blob_head.resolve();
    assert_error(
        &blob_head,
        &release,
        BLOB_FILTER_BYTES,
        "commit inspection failed",
    );
}

fn assert_error(
    repository: &Repository,
    release: &ResolvedWorkflowRelease,
    blob_filter_bytes: u64,
    expected: &str,
) {
    let staging = tempfile::tempdir().expect("temporary staging root");
    let error = fetch_with_options(
        OsStr::new("git"),
        &absolute(staging.path()),
        &repository.source(),
        release,
        /*cancelled*/ None,
        blob_filter_bytes,
    )
    .err()
    .expect("fetch should fail");
    let error = format!("{error:#}");
    assert!(error.contains(expected), "unexpected error: {error}");
}

fn absolute(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path_checked(path).expect("absolute test path")
}

fn git<I, S>(repository: &Path, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("git")
        .env("GIT_AUTHOR_NAME", "Codex Workflow Test")
        .env("GIT_AUTHOR_EMAIL", "workflow-tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "Codex Workflow Test")
        .env("GIT_COMMITTER_EMAIL", "workflow-tests@example.invalid")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .expect("run Git fixture command");
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

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write fake Git");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fake Git executable");
}
