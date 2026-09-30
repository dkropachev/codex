use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn ls_remote_command_is_exact_and_noninteractive() {
    let working_directory = tempfile::tempdir().expect("temporary Git directory");
    let command = ls_remote_command(
        OsStr::new("git"),
        OsStr::new("https://example.com/workflow.git"),
        working_directory.path(),
    );
    assert_eq!(command.get_current_dir(), Some(working_directory.path()));
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        &args[args.len() - 5..],
        [
            "ls-remote",
            "--",
            "https://example.com/workflow.git",
            "HEAD",
            "refs/tags/*"
        ]
    );
    let configurations = args
        .windows(2)
        .filter_map(|args| (args[0] == "-c").then_some(args[1].as_str()))
        .collect::<Vec<_>>();
    let hooks = format!("core.hooksPath={DISABLED_GIT_CONFIG_PATH}");
    assert_eq!(configurations.len(), GIT_CONFIG.len() + 1);
    assert_eq!(&configurations[..GIT_CONFIG.len()], GIT_CONFIG);
    assert_eq!(
        configurations.get(GIT_CONFIG.len()).copied(),
        Some(hooks.as_str())
    );
    let environment = command.get_envs().collect::<BTreeMap<_, _>>();
    let env = |name| {
        environment
            .get(OsStr::new(name))
            .and_then(|value| value.and_then(OsStr::to_str))
    };
    assert_eq!(
        (
            env("GIT_TERMINAL_PROMPT"),
            env("GCM_INTERACTIVE"),
            env("GIT_SSH_COMMAND"),
            env("GIT_CEILING_DIRECTORIES"),
        ),
        (
            Some("0"),
            Some("never"),
            Some("ssh -oBatchMode=yes"),
            working_directory.path().to_str(),
        )
    );
    assert_eq!(env("GIT_TRACE"), None);
    let isolated_git = working_directory.path().join("isolated.git");
    assert_eq!(env("GIT_DIR"), isolated_git.to_str());
    assert_eq!(
        environment.get(OsStr::new("OPENAI_IDENTITY_TOKEN_FILE")),
        Some(&None)
    );
    let expected = [
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", DISABLED_GIT_CONFIG_PATH),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_SYSTEM", DISABLED_GIT_CONFIG_PATH),
        ("GIT_LFS_SKIP_SMUDGE", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("LC_ALL", "C"),
    ];
    for (name, value) in expected {
        assert_eq!(env(name), Some(value), "unexpected {name}");
    }
}

#[test]
fn recognizes_ambient_git_and_askpass_variables() {
    for name in ["GIT_CONFIG_COUNT", "git_trace", "SSH_ASKPASS_REQUIRE"] {
        assert!(is_git_environment_variable(OsStr::new(name)));
    }
    assert!(!is_git_environment_variable(OsStr::new("PATH")));

    let mut command = Command::new("git");
    remove_git_environment_variables(
        &mut command,
        [
            "GIT_TRACE",
            "git_config_count",
            "SSH_ASKPASS_REQUIRE",
            "PATH",
        ]
        .map(OsString::from),
    );
    let environment = command.get_envs().collect::<BTreeMap<_, _>>();
    for name in ["GIT_TRACE", "git_config_count", "SSH_ASKPASS_REQUIRE"] {
        assert_eq!(environment.get(OsStr::new(name)), Some(&None));
    }
    assert!(!environment.contains_key(OsStr::new("PATH")));
}

#[cfg(unix)]
#[test]
fn cancelled_resolution_does_not_spawn_git() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary directory");
    let sentinel = directory.path().join("spawned");
    let fake_git = directory.path().join("git");
    fs::write(
        &fake_git,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", sentinel.display()),
    )
    .expect("write fake Git");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))
        .expect("make fake Git executable");
    let source = WorkflowGitSource::parse("ssh://git@example.com/workflow.git")
        .expect("parse workflow source");
    let cancelled = AtomicBool::new(true);
    let error =
        resolve_workflow_git_release_with_git(fake_git.as_os_str(), &source, Some(&cancelled))
            .expect_err("pre-cancelled resolution should fail");
    assert!(format!("{error:#}").contains("cancelled"));
    assert!(!sentinel.exists());
}

#[cfg(unix)]
#[test]
fn running_resolution_observes_cancellation_and_output_limit() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Instant;

    let directory = tempfile::tempdir().expect("temporary directory");
    let marker = directory.path().join("started");
    let fake_git = directory.path().join("git");
    write_executable(
        &fake_git,
        &format!("touch '{}'; while :; do sleep 1; done", marker.display()),
    );
    let source = WorkflowGitSource::parse("https://example.com/workflow.git")
        .expect("parse workflow source");
    let cancelled = Arc::new(AtomicBool::new(false));
    let task_cancelled = Arc::clone(&cancelled);
    let task_marker = marker.clone();
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !task_marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        task_cancelled.store(true, Ordering::Relaxed);
    });
    let error = resolve_workflow_git_release_with_git(
        fake_git.as_os_str(),
        &source,
        Some(cancelled.as_ref()),
    )
    .expect_err("running resolution should observe cancellation");
    task.join().expect("join cancellation task");
    assert!(marker.exists());
    assert!(format!("{error:#}").contains("cancelled"));

    write_executable(&fake_git, "printf '12345678'");
    let error = resolve_workflow_git_release_with_options(
        fake_git.as_os_str(),
        &source,
        /*cancelled*/ None,
        Duration::from_secs(2),
        /*maximum_stdout_bytes*/ 4,
    )
    .expect_err("oversized release metadata should fail");
    assert!(error.to_string().contains("exceeded 4 bytes"));
}

#[cfg(unix)]
#[test]
fn resolution_reports_bounded_command_failures_without_stderr_secrets() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let fake_git = directory.path().join("git");
    let source = WorkflowGitSource::parse("https://example.com/workflow.git")
        .expect("parse workflow source");

    let missing = directory.path().join("missing-git");
    let error = resolve_workflow_git_release_with_git(
        missing.as_os_str(),
        &source,
        /*cancelled*/ None,
    )
    .expect_err("missing Git should fail");
    assert!(format!("{error:#}").contains("could not start or complete"));

    write_executable(&fake_git, "echo 'server-secret' >&2; exit 7");
    let error = resolve_workflow_git_release_with_git(
        fake_git.as_os_str(),
        &source,
        /*cancelled*/ None,
    )
    .expect_err("failed Git should fail resolution");
    assert_eq!(
        error.to_string(),
        "Git release check failed with exit status 7"
    );
    assert!(!error.to_string().contains("server-secret"));

    write_executable(&fake_git, "printf '\\377'");
    let error = resolve_workflow_git_release_with_git(
        fake_git.as_os_str(),
        &source,
        /*cancelled*/ None,
    )
    .expect_err("non-UTF-8 Git output should fail");
    assert!(error.to_string().contains("non-UTF-8 release metadata"));
}

#[test]
fn resolves_annotated_stable_tag_from_local_repository() {
    let repository = tempfile::tempdir().expect("temporary repository");
    fs::write(repository.path().join("README.md"), "workflow\n").expect("write fixture");
    for args in [
        &["init"][..],
        &["config", "user.email", "codex@example.com"],
        &["config", "user.name", "Codex Test"],
        &["add", "README.md"],
        &["commit", "-m", "initial"],
        &["tag", "v1.1.0"],
        &["tag", "-a", "v1.2.0", "-m", "release"],
        &["tag", "v2.0.0-rc.1"],
    ] {
        run_git(repository.path(), args);
    }
    let head = String::from_utf8(run_git(repository.path(), &["rev-parse", "HEAD"]).stdout)
        .expect("commit is UTF-8")
        .trim()
        .to_string();

    let path = repository
        .path()
        .to_str()
        .expect("repository path is UTF-8");
    let source = WorkflowGitSource::parse(path).expect("parse local source");
    let resolved =
        resolve_workflow_git_release(&source, /*cancelled*/ None).expect("resolve local release");
    assert_eq!(
        resolved,
        ResolvedWorkflowRelease {
            tag: Some("v1.2.0".to_string()),
            version: Some(semver::Version::new(1, 2, 0)),
            advertised_object_id: head.clone(),
        }
    );

    run_git(
        repository.path(),
        &["tag", "-d", "v1.1.0", "v1.2.0", "v2.0.0-rc.1"],
    );
    assert_eq!(
        resolve_workflow_git_release(&source, /*cancelled*/ None).expect("resolve HEAD snapshot"),
        ResolvedWorkflowRelease {
            tag: None,
            version: None,
            advertised_object_id: head,
        }
    );
}

fn run_git(repository: &std::path::Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[cfg(unix)]
fn write_executable(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write fake Git");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("make fake Git executable");
}
