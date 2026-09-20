use std::collections::VecDeque;
use std::sync::Mutex;

use anyhow::Result;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::ReviewCommand;
use crate::ReviewCommandOutput;

#[tokio::test]
async fn detects_uncommitted_changes_from_porcelain_status() {
    for (cwd, hooks_path) in [
        (cwd(), "/dev/null"),
        (PathUri::parse("file:///C:/repo").unwrap(), "NUL"),
    ] {
        let runner = FakeRunner::new(vec![
            response(filter_config_args(), /*exit_code*/ 1, ""),
            response(
                safe_args(
                    hooks_path,
                    &[
                        "status",
                        "--porcelain=v1",
                        "--untracked-files=all",
                        "--ignore-submodules=dirty",
                    ],
                ),
                /*exit_code*/ 0,
                "?? new.rs\n",
            ),
        ]);

        assert!(has_uncommitted_changes(&runner, &cwd).await.unwrap());
    }
}

#[tokio::test]
async fn worktree_scan_rejects_truncated_filter_config() {
    let runner = FakeRunner::new(vec![response(
        filter_config_args(),
        /*exit_code*/ 0,
        &"x".repeat(REVIEW_COMMAND_OUTPUT_BYTES_CAP),
    )]);
    assert!(has_uncommitted_changes(&runner, &cwd()).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn worktree_scan_ignores_dirty_submodule_helpers() {
    let submodule = tempfile::TempDir::new().expect("submodule repository");
    init_filter_repository(submodule.path());

    let repository = tempfile::TempDir::new().expect("parent repository");
    init_native_repository(repository.path());
    let submodule_path = submodule.path().to_str().expect("submodule path");
    run_native(
        repository.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            submodule_path,
            "dependency",
        ],
    );
    run_native(
        repository.path(),
        &["commit", "--no-gpg-sign", "-am", "submodule"],
    );
    let marker = submodule.path().join("filter-ran");
    let filter = submodule.path().join("filter.sh");
    let dependency = repository.path().join("dependency");
    install_filter(&dependency, &filter, &marker);
    std::fs::write(dependency.join("tracked.txt"), "changed\n").expect("change submodule file");
    let cwd = PathUri::from_host_native_path(repository.path()).expect("repository URI");

    assert!(!has_uncommitted_changes(&NativeRunner, &cwd).await.unwrap());
    assert!(!marker.exists(), "submodule clean filter executed");
}

#[tokio::test]
async fn recent_review_commits_are_hard_capped() {
    let stdout = (0..=REVIEW_SCOPE_COMMIT_LIMIT)
        .map(|index| format!("sha-{index}\u{001f}{index}\u{001f}subject {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let runner = FakeRunner::new(vec![response(
        [
            "git",
            "-c",
            "log.showSignature=false",
            "log",
            "-n",
            "100",
            "--pretty=format:%H%x1f%ct%x1f%s",
        ],
        /*exit_code*/ 0,
        &stdout,
    )]);

    let commits = recent_review_commits(&runner, &cwd())
        .await
        .expect("recent commits");

    assert_eq!(commits.len(), REVIEW_SCOPE_COMMIT_LIMIT);
}

#[cfg(unix)]
#[tokio::test]
async fn worktree_scan_disables_repository_helpers() {
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::TempDir::new().expect("temp repo");
    let root_path = root.path();
    init_filter_repository(root_path);
    let fsmonitor_marker = root_path.join("fsmonitor-ran");
    let helper = root_path.join("fsmonitor.sh");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf '0\\n'\n",
            fsmonitor_marker.display()
        ),
    )
    .expect("write helper");
    make_executable(&helper);
    set_config(
        root_path,
        "core.fsmonitor",
        helper.to_str().expect("helper path"),
    );
    let filter_marker = root_path.join("filter-ran");
    let filter = root_path.join("filter.sh");
    let filter_path = filter.to_str().expect("filter path");
    install_filter(root_path, &filter, &filter_marker);
    std::fs::write(root_path.join("tracked.txt"), "changed\n").expect("change tracked file");
    let cwd = PathUri::from_host_native_path(root_path).expect("repo URI");

    assert!(has_uncommitted_changes(&NativeRunner, &cwd).await.unwrap());
    assert!(!fsmonitor_marker.exists(), "fsmonitor helper executed");
    assert!(!filter_marker.exists(), "clean filter executed");

    std::fs::write(root_path.join(".gitattributes"), b"*.txt filter=\xff\n")
        .expect("write non-UTF-8 attributes");
    run_native(root_path, &["config", "--unset-all", "filter.evil.clean"]);
    run_native(
        root_path,
        &["config", "--unset-all", "filter.evil.required"],
    );
    std::process::Command::new("git")
        .arg("-C")
        .arg(root_path)
        .arg("config")
        .arg(std::ffi::OsString::from_vec(b"filter.\xff.clean".to_vec()))
        .arg(&filter)
        .output()
        .expect("configure non-UTF-8 filter");
    assert!(has_uncommitted_changes(&NativeRunner, &cwd).await.is_err());
    assert!(!filter_marker.exists(), "non-UTF-8 clean filter executed");

    std::fs::write(root_path.join(".gitattributes"), "*.txt filter=evil\n")
        .expect("restore attributes");
    std::fs::write(
        &filter,
        format!(
            "#!/bin/sh\ntouch '{}'\necho '[GNUPG:] SIG_CREATED D 1 10 00 0 ABCDEF' >&2\nprintf -- '-----BEGIN PGP SIGNATURE-----\\nZmFrZQ==\\n-----END PGP SIGNATURE-----\\n'\n",
            filter_marker.display()
        ),
    )
    .expect("write fake gpg");
    set_config(root_path, "gpg.format", "openpgp");
    set_config(root_path, "gpg.program", filter_path);
    set_config(root_path, "user.signingkey", "test");
    run_native(root_path, &["commit", "-S", "-am", "signed"]);
    std::fs::remove_file(&filter_marker).expect("clear signing marker");
    set_config(root_path, "log.showSignature", "true");
    assert!(recent_review_commits(&NativeRunner, &cwd).await.is_ok());
    assert!(!filter_marker.exists(), "gpg verifier executed");
}

fn cwd() -> PathUri {
    PathUri::parse("file:///repo").expect("cwd")
}

fn safe_args(disabled_hooks: &str, args: &[&str]) -> Vec<String> {
    std::iter::once("git")
        .map(str::to_string)
        .chain([
            "-c".to_string(),
            format!("core.hooksPath={disabled_hooks}"),
            "-c".to_string(),
            "core.fsmonitor=false".to_string(),
        ])
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .collect()
}

fn filter_config_args() -> Vec<String> {
    [
        "git",
        "config",
        "--null",
        "--name-only",
        "--get-regexp",
        EXECUTABLE_FILTER_CONFIG_PATTERN,
    ]
    .map(str::to_string)
    .into()
}

struct NativeRunner;

impl crate::ReviewCommandRunner for NativeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let cwd = command.cwd().to_abs_path()?;
        let output = tokio::process::Command::new(&command.argv()[0])
            .args(&command.argv()[1..])
            .current_dir(cwd.as_path())
            .envs(command.env_vars())
            .output()
            .await?;
        Ok(ReviewCommandOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_native(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn set_config(cwd: &std::path::Path, key: &str, value: &str) {
    run_native(cwd, &["config", key, value]);
}

#[cfg(unix)]
fn make_executable(path: &std::path::Path) {
    let permissions = std::fs::Permissions::from_mode(/*mode*/ 0o755);
    std::fs::set_permissions(path, permissions).expect("set executable permissions");
}

#[cfg(unix)]
fn configure_filter(cwd: &std::path::Path, filter: &std::path::Path) {
    let filter = filter.to_str().expect("filter path");
    set_config(cwd, "filter.evil.clean", filter);
    set_config(cwd, "filter.evil.required", "true");
}

#[cfg(unix)]
fn install_filter(cwd: &std::path::Path, filter: &std::path::Path, marker: &std::path::Path) {
    std::fs::write(
        filter,
        format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    )
    .expect("write filter");
    make_executable(filter);
    configure_filter(cwd, filter);
}

fn init_native_repository(path: &std::path::Path) {
    run_native(path, &["init"]);
    set_config(path, "user.name", "Codex Test");
    set_config(path, "user.email", "codex@example.com");
    set_config(path, "core.hooksPath", "hooks-disabled");
}

fn init_filter_repository(path: &std::path::Path) {
    init_native_repository(path);
    std::fs::write(path.join(".gitattributes"), "*.txt filter=evil\n").expect("attributes");
    std::fs::write(path.join("tracked.txt"), "tracked\n").expect("tracked file");
    run_native(path, &["add", ".gitattributes", "tracked.txt"]);
    run_native(path, &["commit", "--no-gpg-sign", "-m", "base"]);
}

fn response(
    argv: impl IntoIterator<Item = impl ToString>,
    exit_code: i32,
    stdout: &str,
) -> FakeResponse {
    FakeResponse {
        argv: argv.into_iter().map(|arg| arg.to_string()).collect(),
        output: ReviewCommandOutput {
            exit_code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        },
    }
}

struct FakeResponse {
    argv: Vec<String>,
    output: ReviewCommandOutput,
}

struct FakeRunner {
    responses: Mutex<VecDeque<FakeResponse>>,
}

impl FakeRunner {
    fn new(responses: Vec<FakeResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

impl ReviewCommandRunner for FakeRunner {
    async fn run(&self, command: ReviewCommand) -> Result<ReviewCommandOutput> {
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .expect("unexpected review command");
        assert_eq!(command.argv(), response.argv);
        Ok(response.output)
    }
}
