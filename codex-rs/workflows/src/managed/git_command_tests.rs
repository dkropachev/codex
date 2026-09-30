use std::collections::BTreeMap;
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
}

#[test]
fn recognizes_ambient_git_and_askpass_variables() {
    for name in ["GIT_CONFIG_COUNT", "git_trace", "SSH_ASKPASS_REQUIRE"] {
        assert!(is_git_environment_variable(OsStr::new(name)));
    }
    assert!(!is_git_environment_variable(OsStr::new("PATH")));
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
