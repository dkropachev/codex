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
        if matches!(&kind, ReleaseKind::Annotated) {
            git(
                repository.root(),
                ["config", "uploadpack.allowFilter", "true"],
            );
        }
        repository.tag(&kind);
        let release = repository.resolve();
        let staging = tempfile::tempdir().expect("temporary staging root");
        let command = fetch_command(
            OsStr::new("git"),
            staging.path(),
            &staging.path().join("repository"),
            &release,
            VERIFICATION_LIMITS.blob_bytes + 1,
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
            VERIFICATION_LIMITS,
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
        assert!(
            !fetched
                .repository
                .join(".git/objects/info/alternates")
                .exists()
        );
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
        VERIFICATION_LIMITS,
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
        VERIFICATION_LIMITS,
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
        VERIFICATION_LIMITS,
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
        VERIFICATION_LIMITS,
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
        VERIFICATION_LIMITS,
        "commit inspection failed",
    );

    let mutable = Repository::new("sha1");
    fs::create_dir(mutable.root().join("state")).expect("create state directory");
    fs::write(mutable.root().join("state/session"), "runtime\n").expect("write runtime file");
    git(mutable.root(), ["add", "state/session"]);
    git(
        mutable.root(),
        ["commit", "--no-gpg-sign", "-qm", "runtime"],
    );
    let release = mutable.resolve();
    assert_error(&mutable, &release, VERIFICATION_LIMITS, "runtime path");
}

#[test]
fn rejects_missing_and_oversized_objects_with_or_without_filter_support() {
    for allow_filter in [true, false] {
        let repository = Repository::new("sha1");
        if allow_filter {
            git(
                repository.root(),
                ["config", "uploadpack.allowFilter", "true"],
            );
        } else {
            git(
                repository.root(),
                ["config", "uploadpack.allowFilter", "false"],
            );
        }
        fs::write(repository.root().join("large"), vec![b'x'; 64]).expect("write large blob");
        git(repository.root(), ["add", "large"]);
        git(
            repository.root(),
            ["commit", "--no-gpg-sign", "-qm", "large"],
        );
        let release = repository.resolve();
        let expected = if allow_filter {
            "object verification failed"
        } else {
            "blob exceeds 31 bytes"
        };
        assert_error(
            &repository,
            &release,
            VerificationLimits {
                blob_bytes: 31,
                ..VERIFICATION_LIMITS
            },
            expected,
        );
    }
}

#[test]
fn enforces_logical_object_and_retained_staging_limits() {
    let listing = b"blob 4\ntree 3\n";
    validate_object_listing(
        listing,
        VerificationLimits {
            blob_bytes: 4,
            object_entries: 2,
            object_bytes: 7,
            ..VERIFICATION_LIMITS
        },
        /*cancelled*/ None,
    )
    .expect("exact object limits should pass");
    let error = validate_object_listing(
        listing,
        VerificationLimits {
            blob_bytes: 3,
            ..VERIFICATION_LIMITS
        },
        /*cancelled*/ None,
    )
    .expect_err("blob limit should fail");
    assert!(format!("{error:#}").contains("blob exceeds 3 bytes"));

    let cancelled = AtomicBool::new(true);
    let error = validate_object_listing(listing, VERIFICATION_LIMITS, Some(&cancelled))
        .expect_err("cancelled object inspection should fail");
    assert!(format!("{error:#}").contains("cancelled"));

    let staging = tempfile::tempdir().expect("temporary staging directory");
    fs::create_dir(staging.path().join("nested")).expect("create nested directory");
    fs::write(staging.path().join("nested/data"), "data").expect("write staged data");
    inspect_staging(
        staging.path(),
        VerificationLimits {
            staging_entries: 2,
            staging_bytes: 4,
            ..VERIFICATION_LIMITS
        },
        /*cancelled*/ None,
    )
    .expect("exact staging limits should pass");
    let error = inspect_staging(staging.path(), VERIFICATION_LIMITS, Some(&cancelled))
        .expect_err("cancelled staging inspection should fail");
    assert!(format!("{error:#}").contains("cancelled"));

    let repository = Repository::new("sha1");
    let release = repository.resolve();
    for (limits, expected) in [
        (
            VerificationLimits {
                object_entries: 1,
                ..VERIFICATION_LIMITS
            },
            "object count exceeds 1",
        ),
        (
            VerificationLimits {
                object_bytes: 1,
                ..VERIFICATION_LIMITS
            },
            "object set exceeds 1 bytes",
        ),
        (
            VerificationLimits {
                staging_entries: 0,
                ..VERIFICATION_LIMITS
            },
            "staging exceeds 0 entries",
        ),
        (
            VerificationLimits {
                staging_bytes: 0,
                ..VERIFICATION_LIMITS
            },
            "staging exceeds 0 bytes",
        ),
    ] {
        assert_error(&repository, &release, limits, expected);
    }
}

#[cfg(unix)]
#[test]
fn retained_staging_scan_rejects_symlinks_and_special_files() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let staging = tempfile::tempdir().expect("temporary staging directory");
    let nested = staging.path().join("nested");
    fs::create_dir(&nested).expect("create nested staging directory");
    let outside = tempfile::NamedTempFile::new().expect("outside file");
    symlink(outside.path(), nested.join("link")).expect("create symlink");
    let error = inspect_staging(staging.path(), VERIFICATION_LIMITS, /*cancelled*/ None)
        .expect_err("symlink should fail");
    assert!(format!("{error:#}").contains("symbolic link"));

    fs::remove_file(nested.join("link")).expect("remove symlink");
    let _socket = UnixListener::bind(nested.join("socket")).expect("create socket");
    let error = inspect_staging(staging.path(), VERIFICATION_LIMITS, /*cancelled*/ None)
        .expect_err("special file should fail");
    assert!(format!("{error:#}").contains("special file"));
}

#[test]
fn isolation_check_rejects_network_capable_repository_state() {
    let repository = Repository::new("sha1");
    let staging = tempfile::tempdir().expect("temporary staging root");
    let fetched = fetch_with_options(
        OsStr::new("git"),
        &absolute(staging.path()),
        &repository.source(),
        &repository.resolve(),
        /*cancelled*/ None,
        VERIFICATION_LIMITS,
    )
    .expect("fetch release");
    let root = &fetched.repository;

    for (set_args, cleanup_args, expected) in [
        (
            vec!["config", "remote.evil.url", "https://example.com/repo"],
            vec!["config", "--remove-section", "remote.evil"],
            "remote Git configuration",
        ),
        (
            vec!["config", "include.path", "outside.config"],
            vec!["config", "--unset-all", "include.path"],
            "remote Git configuration",
        ),
        (
            vec!["config", "includeIf.gitdir:/tmp/.path", "outside.config"],
            vec!["config", "--unset-all", "includeIf.gitdir:/tmp/.path"],
            "remote Git configuration",
        ),
        (
            vec!["config", "extensions.partialClone", "evil"],
            vec!["config", "--unset-all", "extensions.partialClone"],
            "remote Git configuration",
        ),
        (
            vec!["config", "extensions.worktreeConfig", "true"],
            vec!["config", "--unset-all", "extensions.worktreeConfig"],
            "remote Git configuration",
        ),
        (
            vec!["update-ref", "refs/heads/extra", RELEASE_REF],
            vec!["update-ref", "-d", "refs/heads/extra"],
            "unexpected Git references",
        ),
    ] {
        git(root, set_args);
        let error = verify_isolated(OsStr::new("git"), root, root, /*cancelled*/ None)
            .expect_err("forbidden repository state should fail");
        assert!(format!("{error:#}").contains(expected));
        git(root, cleanup_args);
    }

    for name in ["alternates", "http-alternates"] {
        let path = root.join(".git/objects/info").join(name);
        fs::write(&path, "outside\n").expect("write alternate fixture");
        let error = verify_isolated(OsStr::new("git"), root, root, /*cancelled*/ None)
            .expect_err("alternate object store should fail");
        assert!(format!("{error:#}").contains("alternate Git objects"));
        fs::remove_file(path).expect("remove alternate fixture");
    }
}

#[cfg(windows)]
#[test]
fn rejects_windows_junctions_as_roots_and_nested_entries() {
    let repository = Repository::new("sha1");
    let parent = tempfile::tempdir().expect("temporary staging parent");
    let target = parent.path().join("target");
    let junction = parent.path().join("junction");
    fs::create_dir(&target).expect("create junction target");
    let output = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .expect("create directory junction");
    assert!(output.status.success(), "mklink /J failed");

    let error = fetch_with_options(
        OsStr::new("git"),
        &absolute(&junction),
        &repository.source(),
        &repository.resolve(),
        /*cancelled*/ None,
        VERIFICATION_LIMITS,
    )
    .err()
    .expect("junction root should fail");
    assert!(format!("{error:#}").contains("regular directory"));
    let error = inspect_staging(parent.path(), VERIFICATION_LIMITS, /*cancelled*/ None)
        .expect_err("nested junction should fail");
    assert!(format!("{error:#}").contains("reparse point"));
    fs::remove_dir(junction).expect("remove directory junction");
}

fn assert_error(
    repository: &Repository,
    release: &ResolvedWorkflowRelease,
    limits: VerificationLimits,
    expected: &str,
) {
    let staging = tempfile::tempdir().expect("temporary staging root");
    let error = fetch_with_options(
        OsStr::new("git"),
        &absolute(staging.path()),
        &repository.source(),
        release,
        /*cancelled*/ None,
        limits,
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
